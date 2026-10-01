//! `task_create_task` RPC 方法：创建任务并立即返回任务 ID

use crate::rpc::TaskManager;
use crate::types::{TaskEvent, TaskEventType};
use jsonrpsee::core::RpcResult;
use ng_core::error::NodegetError;
use ng_core::permission::data_structure::{Permission, Scope, Task};
use ng_core::permission::token_auth::TokenOrAuth;
use ng_core::utils::generate_random_string;
use ng_db::entity::task;
use ng_db::rpc::RpcHelper;
use ng_db::rpc::to_rpc_error;
use sea_orm::{ActiveValue, EntityTrait, Set};
use serde_json::value::RawValue;
use std::sync::Arc;
use tracing::{debug, error, warn};
use uuid::Uuid;

/// 校验任务类型参数是否合法
///
/// 当前仅检查 `Execute` 类型：cmd 不能为空字符串
pub fn validate_task_type(task_type: &TaskEventType) -> anyhow::Result<()> {
    if let TaskEventType::Execute(execute_task) = task_type
        && execute_task.cmd.trim().is_empty()
    {
        return Err(NodegetError::InvalidInput("Execute cmd cannot be empty".to_owned()).into());
    }

    Ok(())
}

/// 创建任务并立即返回任务 ID
///
/// - `manager` — 任务广播管理器，用于向 Agent 推送任务事件
/// - `token` — 身份令牌，需拥有 `Task::Create` 权限
/// - `target_uuid` — 目标 Agent UUID
/// - `task_type` — 任务类型及其参数
///
/// 返回 `{"id": task_id}`。若 Agent 不在线则回滚数据库记录并返回错误。
///
/// 内部步骤：
/// 1. 校验任务类型参数
/// 2. 解析 Token 并检查 `Task::Create` 权限
/// 3. 生成随机 task_token，插入数据库记录
/// 4. 确保 Agent UUID 在 monitoring_uuid 表中注册
/// 5. 通过 `TaskManager::send_event` 推送给 Agent
/// 6. 发送失败则回滚数据库记录
pub async fn create_task(
    manager: &Arc<TaskManager>,
    token: String,
    target_uuid: Uuid,
    task_type: TaskEventType,
) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        validate_task_type(&task_type)?;

        let task_name = task_type.task_name();

        let token_or_auth = TokenOrAuth::from_full_token(&token)
            .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

        let provider = ng_core::permission::permission_checker::get_permission_checker()
            .ok_or_else(|| {
                NodegetError::ConfigNotFound("PermissionChecker not initialized".to_owned())
            })?;

        let is_allowed = provider
            .check_token_limit(
                &token_or_auth,
                &[Scope::AgentUuid(target_uuid)],
                &[Permission::Task(Task::Create(task_name.to_string()))],
            )
            .await?;

        if !is_allowed {
            return Err(NodegetError::PermissionDenied(format!(
                "Permission Denied: Missing Task Create ({task_name}) permission for this Agent"
            ))
            .into());
        }

        let db = crate::rpc::TaskRpcImpl::get_db()?;
        let token = generate_random_string(10);

        let in_data = task::ActiveModel {
            id: ActiveValue::default(),
            uuid: Set(target_uuid),
            token: Set(token.clone()),
            cron_source: Set(None),
            timestamp: Set(None),
            success: Set(None),
            error_message: Set(None),
            task_event_type: crate::rpc::TaskRpcImpl::try_set_json(task_type.clone())
                .map_err(|e| NodegetError::SerializationError(e.to_string()))?,
            task_event_result: Set(None),
            // 派发时间，供保留期清理判断过期（`timestamp` 是结果回传时间，pending 行为 NULL）
            created_at: Set(Some(crate::now_millis())),
        };

        debug!(target: "task", uuid = %target_uuid, "Received task");

        let result = task::Entity::insert(in_data).exec(db).await.map_err(|e| {
            error!(target: "task", error = %e, "Database insert error");
            NodegetError::DatabaseError(format!("Database insert error: {e}"))
        })?;

        let task_id = result.last_insert_id;
        debug!(target: "task", id = task_id, "Task created");

        // Ensure the uuid is registered in the monitoring_uuid table (authoritative Agent table)
        if let Some(uuid_provider) = crate::rpc::monitoring_uuid_provider() {
            if let Err(e) = uuid_provider.get_or_insert(target_uuid).await {
                warn!(target: "task", uuid = %target_uuid, error = %e, "Failed to register monitoring_uuid; agent will re-register on next report");
            }
        }

        let task = TaskEvent {
            task_id: task_id.cast_unsigned(),
            task_token: token,
            task_event_type: task_type,
        };

        match manager.send_event(target_uuid, task).await {
            Ok(()) => {
                let json_str = format!("{{\"id\":{task_id}}}");
                RawValue::from_string(json_str)
                    .map_err(|e| NodegetError::SerializationError(e.to_string()).into())
            }
            Err(e) => {
                // 回滚刚创建的 task 行。回滚失败对调用方原本不可见（错误被 let _ = 丢弃），
                // 现把回滚失败信息附进错误消息，便于运维据消息定位需手动清理的残留 task 行。
                let rollback_failed = match task::Entity::delete_by_id(task_id).exec(db).await {
                    Ok(_) => false,
                    Err(del_err) => {
                        error!(
                            target: "task",
                            task_id,
                            error = %del_err,
                            "Database delete error during rollback (task row may need manual cleanup)"
                        );
                        true
                    }
                };
                error!(target: "task", error = %e.1, "Error sending task event");
                let mut msg = format!("Error sending task event: {}", e.1);
                if rollback_failed {
                    msg.push_str(
                        "; rollback also failed, see logs (task row may need manual cleanup)",
                    );
                }
                Err(NodegetError::AgentConnectionError(msg).into())
            }
        }
    };

    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => Err(to_rpc_error(&e)),
    }
}
