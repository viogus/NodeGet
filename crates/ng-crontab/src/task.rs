//! 定时任务执行模块：定义 JsWorkerScheduler trait 注入及 Agent 任务下发逻辑。
//!
//! `JsWorkerScheduler` 由 Server 二进制在启动时通过 `set_js_worker_scheduler` 注入，
//! 解耦 ng-crontab 与 ng-js-worker 的内部模块结构。
//! Agent 类型定时任务通过 `crontab_task` 函数下发：逐个 UUID 创建 Task 记录，
//! 发送 TaskEvent，失败时回滚已插入的记录，最终写入 CrontabResult。

use crate::rpc::crontab::CrontabRpcImpl;
use ng_core::error::NodegetError;
use ng_core::utils::generate_random_string;
use ng_db::entity::{crontab_result, task};
use ng_db::get_db;
use ng_infra::server::RpcHelper;
use ng_js_runtime::RunType;
use ng_task::{TaskEvent, TaskEventType, TaskManager};
use sea_orm::{ActiveValue, EntityTrait, Set};
use tracing::{Instrument, debug, error, info, info_span, warn};
use uuid::Uuid;

// ── JsWorkerScheduler trait 注入 ─────────────────────────────────────

/// JS Worker 调度器 trait，由 Server 层注入具体实现。
///
/// ng-js-worker crate 提供具体实现，包装 `enqueue_defined_js_worker_run`，
/// 解耦 ng-crontab 与 ng-js-worker 的内部模块结构。
pub trait JsWorkerScheduler: Send + Sync + 'static {
    /// 将 JS Worker 运行请求加入调度队列。
    ///
    /// - `worker_name` - JS Worker 脚本名称
    /// - `run_type` - 运行类型（Cron / Manual 等）
    /// - `params` - 传入参数 JSON
    /// - `env_override` - 环境变量覆盖（可选）
    /// - 返回关联的 relative_id
    fn enqueue_run(
        &self,
        worker_name: String,
        run_type: RunType,
        params: serde_json::Value,
        env_override: Option<serde_json::Value>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send>>;
}

/// 全局 JsWorkerScheduler 单例，启动时由 Server 二进制通过 `set_js_worker_scheduler` 注入。
static JS_WORKER_SCHEDULER: std::sync::OnceLock<std::sync::Arc<dyn JsWorkerScheduler>> =
    std::sync::OnceLock::new();

/// 设置全局 JS Worker 调度器（启动时调用一次）。
pub fn set_js_worker_scheduler(scheduler: std::sync::Arc<dyn JsWorkerScheduler>) {
    let _ = JS_WORKER_SCHEDULER.set(scheduler);
}

/// 获取全局 JS Worker 调度器。
pub fn js_worker_scheduler() -> Option<&'static std::sync::Arc<dyn JsWorkerScheduler>> {
    JS_WORKER_SCHEDULER.get()
}

// ── Agent 任务下发 ────────────────────────────────────────────────

/// 向指定 Agent UUID 列表下发定时任务。
///
/// 1. 逐个 UUID 生成随机 token 并创建 task 数据库记录
/// 2. 通过 TaskManager 发送 TaskEvent 到对应 Agent
/// 3. 若发送失败，回滚已插入的 task 记录
/// 4. 将每次下发的结果写入 crontab_result 表
///
/// - `cron_id` - 定时任务 ID
/// - `cron_name` - 定时任务名称
/// - `uuids` - 目标 Agent UUID 列表
/// - `task_event_type` - 任务事件类型
pub async fn crontab_task(
    cron_id: i64,
    cron_name: String,
    uuids: Vec<Uuid>,
    task_event_type: TaskEventType,
) {
    let span = info_span!(
        target: "crontab",
        "crontab::dispatch_task",
        cron_id,
        cron_name = %cron_name,
    );

    async {
        let db = match get_db() {
            Some(db) => db,
            None => {
                error!(
                    target: "crontab",
                    "failed to get DB connection for crontab task"
                );
                return;
            }
        };

        let agent_count = uuids.len();
        info!(
            target: "crontab",
            agent_count,
            task_type = ?task_event_type,
            "dispatching task to agents"
        );

        for uuid in uuids {
            // 单个 Agent 的下发逻辑：创建记录 -> 发送事件 -> 失败回滚
            let process_logic =
                async {
                    // 生成随机 token 用于任务认证
                    let token = generate_random_string(10);

                    let in_data = task::ActiveModel {
                        id: ActiveValue::default(),
                        uuid: Set(uuid),
                        token: Set(token.clone()),
                        cron_source: Set(Some(cron_name.clone())),
                        timestamp: Set(None),
                        success: Set(None),
                        error_message: Set(None),
                        task_event_type: <CrontabRpcImpl as RpcHelper>::try_set_json(
                            task_event_type.clone(),
                        )
                        .map_err(|e| NodegetError::SerializationError(format!("{e}")))?,
                        task_event_result: Set(None),
                    };

                    let result = task::Entity::insert(in_data).exec(db).await.map_err(|e| {
                        error!(
                            target: "crontab",
                            agent_uuid = %uuid,
                            error = %e,
                            "database insert error"
                        );
                        NodegetError::DatabaseError(format!("Database insert error: {e}"))
                    })?;

                    let task_id = result.last_insert_id;
                    debug!(
                        target: "crontab",
                        agent_uuid = %uuid,
                        task_id,
                        "task record inserted"
                    );

                    let task = TaskEvent {
                        task_id: task_id.cast_unsigned(),
                        task_token: token,
                        task_event_type: task_event_type.clone(),
                    };

                    let manager = TaskManager::global();

                    // 尝试发送任务事件到 Agent，失败时回滚已插入的记录
                    match manager.send_event(uuid, task).await {
                        Ok(()) => {
                            info!(
                                target: "crontab",
                                agent_uuid = %uuid,
                                task_id,
                                "task event sent to agent"
                            );
                            Ok(task_id)
                        }
                        Err(e) => {
                            // 发送失败：回滚已插入的 task 记录
                            let _ = task::Entity::delete_by_id(task_id).exec(db).await.map_err(
                                |del_err| {
                                    error!(
                                        target: "crontab",
                                        agent_uuid = %uuid,
                                        task_id,
                                        error = %del_err,
                                        "database delete error during rollback"
                                    );
                                    NodegetError::DatabaseError(format!(
                                        "Database delete error: {del_err}"
                                    ))
                                },
                            );
                            error!(
                                target: "crontab",
                                agent_uuid = %uuid,
                                task_id,
                                error = %e.1,
                                "failed to send task event to agent"
                            );
                            Err(NodegetError::AgentConnectionError(format!(
                                "Error sending task event: {}",
                                e.1
                            )))
                        }
                    }
                };

            // 执行下发逻辑并获取结果状态
            let (success, message, task_id) = match process_logic.await {
                Ok(new_id) => (
                    true,
                    format!("任务下发成功，Agent：[{uuid}]，relative_id：{new_id}"),
                    Some(new_id),
                ),
                Err(e) => {
                    warn!(
                        target: "crontab",
                        agent_uuid = %uuid,
                        error = %e,
                        "task dispatch failed"
                    );
                    (
                        false,
                        format!("任务下发失败，Agent：[{uuid}]，错误：{e}"),
                        None,
                    )
                }
            };

            // 写入执行结果到 crontab_result 表
            let crontab_log = crontab_result::ActiveModel {
                id: ActiveValue::NotSet,
                cron_id: Set(cron_id),
                cron_name: Set(cron_name.clone()),
                relative_id: Set(task_id),
                run_time: Set(Some(chrono::Utc::now().timestamp_millis())),
                success: Set(Some(success)),
                message: Set(Some(message)),
            };

            if let Err(e) = crontab_result::Entity::insert(crontab_log).exec(db).await {
                error!(
                    target: "crontab",
                    agent_uuid = %uuid,
                    error = %e,
                    "failed to save crontab_result"
                );
            }
        }
    }
    .instrument(span)
    .await;
}
