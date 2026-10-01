//! `task_create_task_blocking` RPC 方法：创建任务并阻塞等待 Agent 返回结果

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

/// 创建任务并阻塞等待 Agent 返回结果
///
/// 与 `create_task` 的区别：
/// - `create_task` 创建任务后立即返回 `{"id": task_id}`
/// - `create_task_blocking` 创建任务后等待 Agent 上传结果，然后返回完整的任务结果
/// - 如果超时（timeout_ms），返回错误
///
/// - `manager` — 任务广播管理器
/// - `token` — 身份令牌，需同时拥有 `Task::Create` 和 `Task::Read` 权限
/// - `target_uuid` — 目标 Agent UUID
/// - `task_type` — 任务类型及其参数
/// - `timeout_ms` — 等待超时时间（毫秒），上限 300 秒
///
/// 返回 Agent 上传的完整 `TaskEventResponse`。
///
/// 内部步骤：
/// 1. 校验任务类型参数
/// 2. 解析 Token 并检查 `Task::Create` + `Task::Read` 权限
/// 3. 生成随机 task_token，插入数据库记录
/// 4. 确保 Agent UUID 在 monitoring_uuid 表中注册
/// 5. 在 `send_event` 之前注册 blocking waiter（避免竞态）
/// 6. 通过 `TaskManager::send_event` 推送给 Agent
/// 7. 等待 oneshot channel 结果或超时
pub async fn create_task_blocking(
    manager: &Arc<TaskManager>,
    token: String,
    target_uuid: Uuid,
    task_type: TaskEventType,
    timeout_ms: u64,
) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        // 内联 create_task 逻辑，以便在 send_event 之前注册 waiter，避免 Agent 极快返回时错过通知

        super::create_task::validate_task_type(&task_type)?;

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
                &[
                    Permission::Task(Task::Create(task_name.to_string())),
                    Permission::Task(Task::Read(task_name.to_string())),
                ],
            )
            .await?;

        if !is_allowed {
            return Err(NodegetError::PermissionDenied(format!(
                "Permission Denied: Missing Task Create or Read ({task_name}) permission for this Agent"
            ))
                .into());
        }

        let db = crate::rpc::TaskRpcImpl::get_db()?;
        let task_token = generate_random_string(10);

        let in_data = task::ActiveModel {
            id: ActiveValue::default(),
            uuid: Set(target_uuid),
            token: Set(task_token.clone()),
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

        let result = task::Entity::insert(in_data).exec(db).await.map_err(|e| {
            error!(target: "task", error = %e, "Database insert error");
            NodegetError::DatabaseError(format!("Database insert error: {e}"))
        })?;

        let task_id = result.last_insert_id;
        let task_id_u64 = task_id.cast_unsigned();

        debug!(target: "task", task_id = task_id_u64, "task created, registering blocking waiter");

        // Ensure the uuid is registered in the monitoring_uuid table (authoritative Agent table)
        if let Some(uuid_provider) = crate::rpc::monitoring_uuid_provider() {
            if let Err(e) = uuid_provider.get_or_insert(target_uuid).await {
                warn!(target: "task", uuid = %target_uuid, error = %e, "Failed to register monitoring_uuid; agent will re-register on next report");
            }
        }

        // 关键：在 send_event 之前注册 waiter，避免 agent 极快返回时错过通知
        let rx = manager.register_blocking_waiter(task_id_u64);

        let task_event = TaskEvent {
            task_id: task_id_u64,
            task_token,
            task_event_type: task_type,
        };

        if let Err(e) = manager.send_event(target_uuid, task_event).await {
            // 发送失败，清理 waiter 和 DB 记录
            manager.remove_blocking_waiter(task_id_u64);
            // 回滚失败原本完全静默（let _ = 丢弃）；现把失败信息附进错误消息，
            // 便于运维据消息定位需手动清理的残留 task 行。
            let rollback_failed = match task::Entity::delete_by_id(task_id).exec(db).await {
                Ok(_) => false,
                Err(del_err) => {
                    error!(
                        target: "task",
                        task_id = task_id_u64,
                        error = %del_err,
                        "Database delete error during rollback (task row may need manual cleanup)"
                    );
                    true
                }
            };
            error!(target: "task", error = %e.1, "Error sending task event");
            let mut msg = format!("Error sending task event: {}", e.1);
            if rollback_failed {
                msg.push_str("; rollback also failed, see logs (task row may need manual cleanup)");
            }
            return Err(NodegetError::AgentConnectionError(msg).into());
        }

        debug!(target: "task", task_id = task_id_u64, timeout_ms = timeout_ms, "waiting for agent result");

        // 等待结果或超时
        const MAX_TIMEOUT_MS: u64 = 300_000;
        // 下界钳到 1ms：timeout_ms == 0 时 timeout(Duration::ZERO) 首次 poll 即返回超时，
        // 导致任务刚发出就报错并残留 success=NULL 行。0 无业务意义，钳为 1ms。
        let clamped_timeout_ms = timeout_ms.clamp(1, MAX_TIMEOUT_MS);
        let timeout_duration = std::time::Duration::from_millis(clamped_timeout_ms);
        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(response)) => {
                debug!(target: "task", task_id = task_id_u64, success = response.success, "blocking task completed");
                let json_str = serde_json::to_string(&response)
                    .map_err(|e| NodegetError::SerializationError(e.to_string()))?;
                RawValue::from_string(json_str)
                    .map_err(|e| NodegetError::SerializationError(e.to_string()).into())
            }
            Ok(Err(_)) => {
                manager.remove_blocking_waiter(task_id_u64);
                error!(target: "task", task_id = task_id_u64, "blocking waiter channel closed unexpectedly");
                Err(
                    NodegetError::Other("Blocking waiter channel closed unexpectedly".to_owned())
                        .into(),
                )
            }
            Err(_) => {
                manager.remove_blocking_waiter(task_id_u64);
                debug!(target: "task", task_id = task_id_u64, timeout_ms = timeout_ms, "blocking task timed out");
                Err(NodegetError::Other(format!(
                    "Task {task_id_u64} timed out after {timeout_ms}ms"
                ))
                .into())
            }
        }
    };

    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => Err(to_rpc_error(&e)),
    }
}
