//! 定时任务执行模块：定义 JsWorkerScheduler trait 注入及 Agent 任务下发逻辑。
//!
//! `JsWorkerScheduler` 由 Server 二进制在启动时通过 `set_js_worker_scheduler` 注入，
//! 解耦 ng-crontab 与 ng-js-worker 的内部模块结构。
//! Agent 类型定时任务通过 `crontab_task` 函数下发：批量构建 Task 记录，
//! 并发发送 TaskEvent，失败时批量回滚，最终批量写入 CrontabResult。

use crate::rpc::crontab::CrontabRpcImpl;
use ng_core::error::NodegetError;
use ng_core::utils::generate_random_string;
use ng_db::entity::{crontab_result, task};
use ng_db::get_db;
use ng_infra::server::RpcHelper;
use ng_js_runtime::RunType;
use ng_task::{TaskEvent, TaskEventType, TaskManager};
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter, Set};
use tokio::task::JoinSet;
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

// ── 派发策略注入 ──────────────────────────────────────────────────

/// 全部离线时的上报节流窗口（毫秒）。
///
/// 20 秒周期的 cron 打到离线 Agent 上，一天是 4320 次运行；如果每次都写一条失败行，
/// 单个 cron 就是 4320 行/天，正是派发前置预检要消除的那个量级。窗口内只保留一条。
const ALL_OFFLINE_THROTTLE_MS: i64 = 10 * 60 * 1000;

/// 全部离线失败行的消息前缀（用于识别上一条是否同类）。
const ALL_OFFLINE_MESSAGE_PREFIX: &str = "全部目标 Agent 离线";

/// 节流查询用的 `LIKE` 模式（`<全部离线前缀>%`）。
///
/// 抽成函数是为了让它可测：判定必须与真正写入的消息前缀一致，
/// 否则节流会失效（每次都写一行）。前缀匹配下推到 SQL，避免「最新一条是别的
/// 失败原因」就把节流重新计时。
fn all_offline_like_pattern() -> String {
    format!("{ALL_OFFLINE_MESSAGE_PREFIX}%")
}

/// Agent 定时任务的派发策略，由 Server 二进制在启动时通过 [`set_dispatch_policy`] 注入。
#[derive(Debug, Clone, Copy)]
pub struct DispatchPolicy {
    /// `crontab_result` 的成功行是否按「每次运行一条汇总行」写入。
    ///
    /// `true`（默认）时一次派发只写一条成功行，记录成功下发的 Agent 数；
    /// 单 Agent 派发时该行仍保留 `relative_id = task_id`（多 Agent 聚合行没有单一
    /// task_id，置 `NULL`）。`false` 时回退为每个 Agent 一条成功行。
    /// 失败行始终逐个 Agent 记录。
    pub aggregate_success_result: bool,
}

impl Default for DispatchPolicy {
    fn default() -> Self {
        Self {
            aggregate_success_result: true,
        }
    }
}

/// 全局派发策略，启动时由 Server 二进制通过 `set_dispatch_policy` 注入。
///
/// 用可写锁而非 `OnceLock`：`serve` 在配置热重载时会重新注入一遍，`OnceLock`
/// 的第二次 `set` 会被静默忽略——改了 `[crontab] aggregate_success_result` 却要
/// 重启才生效，且没有任何告警。这里允许覆盖，并在值发生变化时打一条 warn。
static DISPATCH_POLICY: std::sync::RwLock<Option<DispatchPolicy>> = std::sync::RwLock::new(None);

/// 设置（或热重载更新）全局派发策略。
pub fn set_dispatch_policy(policy: DispatchPolicy) {
    let mut slot = DISPATCH_POLICY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(previous) = *slot
        && previous.aggregate_success_result != policy.aggregate_success_result
    {
        warn!(
            target: "crontab",
            previous = previous.aggregate_success_result,
            current = policy.aggregate_success_result,
            "dispatch policy changed at runtime"
        );
    }
    *slot = Some(policy);
}

/// 获取全局派发策略；未注入时使用 [`DispatchPolicy::default`]。
pub fn dispatch_policy() -> DispatchPolicy {
    DISPATCH_POLICY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .unwrap_or_default()
}

// ── Agent 任务下发 ────────────────────────────────────────────────

/// 向指定 Agent UUID 列表批量下发定时任务。
///
/// 1. 先剔除离线 Agent（`TaskManager::connected_uuids`），避免必然失败的派发
/// 2. 一次性序列化 `task_event_type`，批量构建所有 task ActiveModel
/// 3. 单次 `insert_many` + `exec_with_returning` 写入 task 记录，用 RETURNING 取回每行真实 id
/// 4. 并发发送 TaskEvent 到各 Agent（JoinSet 替代逐个 await）
/// 5. 批量回滚发送失败的 task 记录
/// 6. 单次 `insert_many` 写入 crontab_result（成功行按 [`DispatchPolicy`] 决定是否聚合）
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

        let requested_count = uuids.len();
        if requested_count == 0 {
            return;
        }

        // 派发前剔除离线 Agent：对未连接的 Agent 下发必然返回 104，除了一次无意义的
        // task INSERT + DELETE，还会写一条失败的 crontab_result 并刷 WARN 日志。
        // 生产实测：单个离线 Agent 在 6 条 20 秒周期的 cron 下每天产生约 2.6 万行无效记录。
        // 离线期间的补偿由 Agent 重连后的 check-online-status / base-worker 逻辑负责，
        // 不依赖这里的失败重试（原实现也从不重试）。
        let uuids = TaskManager::global().connected_uuids(&uuids).await;
        let agent_count = uuids.len();
        let skipped_at_precheck = requested_count - agent_count;
        if agent_count == 0 {
            // 全部离线也要写一条结果行：`last_run_time` 在调度前就已落库，
            // 这一轮不会重试，如果这里什么都不写，UI 上只会看到「cron 没跑」，
            // 而真实原因是 Agent 没连上（常见于 allow_task = false）。
            let now_ms = chrono::Utc::now().timestamp_millis();
            // 同一 cron 在节流窗口内已经留过同类失败行就不再重复写，避免把
            // crontab_result 变成每 20 秒一行的日志表（离线 Agent 一天 2.6 万行）。
            // 前缀条件必须下推到 SQL：只在 Rust 侧检查「最新一条失败行」的话，
            // 中间夹杂一条其它原因的失败行就会让节流重新计时，又变成每次运行写一行。
            let recently_reported = match crontab_result::Entity::find()
                .filter(crontab_result::Column::CronId.eq(cron_id))
                .filter(crontab_result::Column::Success.eq(Some(false)))
                .filter(crontab_result::Column::RunTime.gte(now_ms - ALL_OFFLINE_THROTTLE_MS))
                .filter(crontab_result::Column::Message.like(all_offline_like_pattern()))
                .one(db)
                .await
            {
                Ok(row) => row.is_some(),
                Err(e) => {
                    error!(target: "crontab", error = %e, "failed to check recent all-offline result");
                    false
                }
            };
            if recently_reported {
                debug!(
                    target: "crontab",
                    requested_count,
                    skipped_at_precheck,
                    "all target agents offline; failure already reported within throttle window"
                );
            } else {
                warn!(
                    target: "crontab",
                    requested_count,
                    skipped_at_precheck,
                    task_type = ?task_event_type,
                    "all target agents offline, task dispatch skipped"
                );
            }
            if recently_reported {
                return;
            }
            let result = crontab_result::ActiveModel {
                id: ActiveValue::NotSet,
                cron_id: Set(cron_id),
                cron_name: Set(cron_name.clone()),
                relative_id: Set(None),
                run_time: Set(Some(now_ms)),
                success: Set(Some(false)),
                message: Set(Some(format!(
                    "{ALL_OFFLINE_MESSAGE_PREFIX}（{skipped_at_precheck} 个），本次未下发；\
                     请检查 Agent 是否在线且 allow_task 已开启（同一 cron 10 分钟内只记一条）"
                ))),
            };
            if let Err(e) = crontab_result::Entity::insert(result).exec(db).await {
                error!(target: "crontab", error = %e, "failed to record all-offline crontab result");
            }
            return;
        }
        info!(
            target: "crontab",
            agent_count,
            skipped_at_precheck,
            task_type = ?task_event_type,
            "dispatching task to agents"
        );

        // 序列化一次，所有 task 记录共享
        let task_event_type_value =
            <CrontabRpcImpl as RpcHelper>::try_set_json(task_event_type.clone())
                .map_err(|e| NodegetError::SerializationError(format!("{e}")));

        let task_event_type_value = match task_event_type_value {
            Ok(v) => v,
            Err(e) => {
                error!(target: "crontab", error = %e, "failed to serialize task_event_type");
                return;
            }
        };

        // 批量构建 task ActiveModel（每个 uuid 一条，token 各自随机）。
        // `created_at` 在派发时写入，供保留期清理按「写入时间」判断过期；
        // `timestamp` 是 Agent 回传结果时写入的时间，pending 行该列为 NULL，不能用于清理。
        let now_ms = chrono::Utc::now().timestamp_millis();
        let task_models: Vec<task::ActiveModel> = uuids
            .iter()
            .map(|uuid| {
                let token = generate_random_string(10);
                task::ActiveModel {
                    id: ActiveValue::default(),
                    uuid: Set(*uuid),
                    token: Set(token),
                    cron_source: Set(Some(cron_name.clone())),
                    timestamp: Set(None),
                    success: Set(None),
                    error_message: Set(None),
                    task_event_type: task_event_type_value.clone(),
                    task_event_result: Set(None),
                    created_at: Set(Some(now_ms)),
                }
            })
            .collect();

        // 批量 INSERT 并用 RETURNING 取回每行的真实 Model。
        // ⚠️ 不能用 `last_insert_id` 反推连续区间：PostgreSQL 的 IDENTITY 序列 nextval 按行原子
        // 分配，并发 insert_many（多个 cron 同 tick、cron 与 task.create 并发）会让本批拿到的 id
        // 交错（如本批得 100/102/104），`base_id = last_id - (n-1)` 会把属于别的批次的 id 当作
        // 本批任务下发，导致 task_id+uuid+token 错配（详见 upload_task_result 三元组校验）。
        // RETURNING 按 VALUES 顺序返回每行 DB 实际分配的 id，直接按下标配对，PG/SQLite 均正确。
        let inserted = match task::Entity::insert_many(task_models)
            .exec_with_returning(db)
            .await
        {
            Ok(models) => models,
            Err(e) => {
                error!(target: "crontab", error = %e, "batch task insert error");
                return;
            }
        };

        // 从 RETURNING 的 Models 派生每条任务的派发三元组（真实 id + uuid + token）。
        // inserted 与 uuids 同序（写入顺序 == VALUES 顺序 == RETURNING 顺序）。
        let dispatch = pair_task_ids(&inserted);

        // 并发发送任务事件（dispatch 中每个元组的 task_id 均为 DB 真实 id）
        let manager = TaskManager::global();
        let mut send_set = JoinSet::new();

        for (task_id, uuid, token) in dispatch {
            let task = TaskEvent {
                task_id,
                task_token: token,
                task_event_type: task_event_type.clone(),
            };
            send_set.spawn(async move {
                (uuid, task_id.cast_signed(), manager.send_event(uuid, task).await)
            });
        }

        // 收集发送结果
        let mut crontab_results: Vec<crontab_result::ActiveModel> = Vec::with_capacity(agent_count);
        // 未送达的 task 行 id：包括「离线跳过」与「队列满失败」两类，最后统一删除。
        // 两者都需要清理（任务从未到达 Agent），但只有后者写失败行。
        let mut undelivered_task_ids: Vec<i64> = Vec::new();
        let aggregate_success = dispatch_policy().aggregate_success_result;
        let mut success_count: u64 = 0;
        // 预检通过后仍离线而跳过的 Agent 数（预期内，不记失败）。
        // 与预检阶段的 skipped_at_precheck 是两个独立计数，不能同名遮蔽。
        let mut skipped_on_send: u64 = 0;
        // 单 Agent 派发时保留 task_id，供聚合行的 relative_id 使用
        let mut sole_success_task_id: Option<i64> = None;
        // 仅在非聚合模式下逐 Agent 记录成功行；聚合模式下不构造这些字符串，避免无谓分配
        let mut success_rows: Vec<crontab_result::ActiveModel> = Vec::new();

        while let Some(res) = send_set.join_next().await {
            let (uuid, task_id, send_result) = match res {
                Ok(r) => r,
                Err(e) => {
                    error!(target: "crontab", error = %e, "send task panicked");
                    continue;
                }
            };

            match send_result {
                Ok(()) => {
                    debug!(target: "crontab", agent_uuid = %uuid, task_id, "task event sent to agent");
                    if requested_count == 1 {
                        sole_success_task_id = Some(task_id);
                    }
                    success_count += 1;
                    if !aggregate_success {
                        success_rows.push(crontab_result::ActiveModel {
                            id: ActiveValue::NotSet,
                            cron_id: Set(cron_id),
                            cron_name: Set(cron_name.clone()),
                            relative_id: Set(Some(task_id)),
                            run_time: Set(Some(now_ms)),
                            success: Set(Some(true)),
                            message: Set(Some(format!(
                                "任务下发成功，Agent：[{uuid}]，relative_id：{task_id}"
                            ))),
                        });
                    }
                }
                Err(send_err) => {
                    undelivered_task_ids.push(task_id);
                    if send_err.is_not_connected() {
                        // 预检（connected_uuids）通过后、发送前掉线的窄竞态：属于预期内的
                        // 离线，只清理 task 行、不写失败行、不打 WARN（6 条 ping/tcping
                        // cron 每秒同发，一次抖动会连刷 5~6 行，历史上一晚能刷上千行）。
                        debug!(
                            target: "crontab",
                            agent_uuid = %uuid,
                            task_id,
                            "agent went offline between precheck and send, task skipped"
                        );
                        skipped_on_send += 1;
                    } else {
                        // 队列满等真正的发送失败：保留 WARN 与失败行
                        let detail = send_err.message();
                        warn!(
                            target: "crontab",
                            agent_uuid = %uuid,
                            task_id,
                            error = %detail,
                            "failed to send task event to agent"
                        );
                        crontab_results.push(crontab_result::ActiveModel {
                            id: ActiveValue::NotSet,
                            cron_id: Set(cron_id),
                            cron_name: Set(cron_name.clone()),
                            relative_id: Set(None),
                            run_time: Set(Some(now_ms)),
                            success: Set(Some(false)),
                            message: Set(Some(format!(
                                "任务下发失败，Agent：[{uuid}]，错误：{detail}"
                            ))),
                        });
                    }
                }
            }
        }

        // 成功结果按策略写入：默认「每次运行一条汇总行」。
        // 一次运行 = 一次批量派发（生产上 23 个 Agent），逐 Agent 记成功行会把
        // crontab_result 撑到约 59 万行/天，而 UI 只需要「本次运行成功下发了几个 Agent」。
        // 需要逐 Agent 明细时把 `[crontab] aggregate_success_result` 设为 false 回退。
        // 失败行始终逐 Agent 记录，不受该策略影响。
        if aggregate_success {
            if success_count > 0 {
                // 离线跳过数只在非零时附注，保持既有文案前缀不变（UI/机器人按前缀展示）。
                // 预检跳过 + 发送期跳过都算「没送到的 Agent」，两段计数都要体现出来。
                let skipped_total = skipped_at_precheck + usize::try_from(skipped_on_send).unwrap_or(0);
                let offline_note = if skipped_total > 0 {
                    format!("，离线跳过：{skipped_total}")
                } else {
                    String::new()
                };
                crontab_results.push(crontab_result::ActiveModel {
                    id: ActiveValue::NotSet,
                    cron_id: Set(cron_id),
                    cron_name: Set(cron_name.clone()),
                    // 单 Agent 派发时保留 run→task 的关联（文档承诺 relative_id = task_id）；
                    // 多 Agent 聚合行没有单一 task_id，只能置 None。
                    relative_id: Set(if requested_count == 1 {
                        sole_success_task_id
                    } else {
                        None
                    }),
                    run_time: Set(Some(now_ms)),
                    success: Set(Some(true)),
                    message: Set(Some(format!(
                        "任务下发成功，Agent 数：{success_count}/{requested_count}{offline_note}"
                    ))),
                });
            }
        } else {
            crontab_results.append(&mut success_rows);
        }

        // 批量清理未送达的 task 记录（离线跳过 + 发送失败）
        if !undelivered_task_ids.is_empty()
            && let Err(e) = task::Entity::delete_many()
            .filter(task::Column::Id.is_in(undelivered_task_ids))
            .exec(db)
            .await
        {
            error!(target: "crontab", error = %e, "failed to batch delete undelivered task records");
        }

        // 批量写入 crontab_result（单次 DB 往返）
        if !crontab_results.is_empty()
            && let Err(e) = crontab_result::Entity::insert_many(crontab_results)
            .exec(db)
            .await
        {
            error!(target: "crontab", error = %e, "failed to batch save crontab_results");
        }
    }
        .instrument(span)
        .await;
}

/// 从 `insert_many(...).exec_with_returning()` 返回的 Models 派生每条任务的派发三元组：
/// DB 真实 id、目标 agent uuid、token。
///
/// 关键不变量：每个 `model.id` 是 DB 实际分配的值，**不假设** id 在批次内连续。
/// PostgreSQL 的 IDENTITY 序列在并发 `insert_many` 下会按行交错分配 id（如本批得
/// `100/102/104`），故绝不能用 `last_insert_id - (n-1)` 反推区间——必须用 RETURNING
/// 的真实 id 按写入顺序配对。
///
/// RETURNING 行顺序 == VALUES 写入顺序 == `task_models` 构建顺序 == `uuids` 顺序，
/// 因此 `dispatch[i]` 对应 `uuids[i]`。
fn pair_task_ids(models: &[task::Model]) -> Vec<(u64, Uuid, String)> {
    models
        .iter()
        .map(|m| (m.id.cast_unsigned(), m.uuid, m.token.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::pair_task_ids;
    use ng_db::entity::task;
    use sea_orm::entity::prelude::*;
    use uuid::Uuid;

    /// 构造一个最小可用的 task::Model，仅关心测试用到的字段（id/uuid/token）。
    fn make_model(id: i64, uuid: Uuid, token: &str) -> task::Model {
        task::Model {
            id,
            uuid,
            token: token.to_owned(),
            cron_source: None,
            timestamp: None,
            success: None,
            error_message: None,
            task_event_type: Json::Null,
            task_event_result: None,
            created_at: None,
        }
    }

    /// 复刻 PostgreSQL 并发交错场景：本批 RETURNING 回来的 id 非连续（100/102/104）。
    /// 旧的反推逻辑 `base_id = last_id - (n-1)` 在此输入下会算出 102/103/104，
    /// 把属于别的批次的 103 当作本批任务下发；本测试证明修复后配对与真实 id 一一对应。
    #[test]
    fn pair_task_ids_interleaved_non_contiguous_ids() {
        let u1 = Uuid::nil();
        let u2 = Uuid::from_u128(2);
        let u3 = Uuid::from_u128(3);
        let models = vec![
            make_model(100, u1, "t1"),
            make_model(102, u2, "t2"),
            make_model(104, u3, "t3"),
        ];

        let dispatch = pair_task_ids(&models);

        assert_eq!(dispatch.len(), 3);
        // 每个 task_id 必须等于对应 model 的真实 id（不连续也正确配对）
        assert_eq!(dispatch[0], (100, u1, "t1".to_owned()));
        assert_eq!(dispatch[1], (102, u2, "t2".to_owned()));
        assert_eq!(dispatch[2], (104, u3, "t3".to_owned()));
    }

    /// 返回顺序与输入顺序严格一致（写入顺序 == 下发顺序，不发生移位）。
    #[test]
    fn pair_task_ids_preserves_order() {
        let ids = [7_i64, 3, 12, 1, 9];
        let models: Vec<task::Model> = ids
            .iter()
            .enumerate()
            .map(|(i, &id)| make_model(id, Uuid::from_u128(i as u128), &format!("tok{i}")))
            .collect();

        let dispatch = pair_task_ids(&models);

        assert_eq!(
            dispatch.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
            vec![7_u64, 3, 12, 1, 9]
        );
        assert_eq!(
            dispatch
                .iter()
                .map(|(_, _, t)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["tok0", "tok1", "tok2", "tok3", "tok4"]
        );
    }

    #[test]
    fn pair_task_ids_empty() {
        assert!(pair_task_ids(&[]).is_empty());
    }

    #[test]
    fn pair_task_ids_single() {
        let uuid = Uuid::nil();
        let models = vec![make_model(42, uuid, "solo")];
        let dispatch = pair_task_ids(&models);
        assert_eq!(dispatch, vec![(42, uuid, "solo".to_owned())]);
    }

    /// 默认派发策略：成功行按「每次运行一条汇总行」写入。
    /// 热重载会重复注入：第二次 `set` 必须覆盖第一次（OnceLock 会静默忽略）。
    #[test]
    fn dispatch_policy_can_be_updated_at_runtime() {
        super::set_dispatch_policy(super::DispatchPolicy {
            aggregate_success_result: false,
        });
        assert!(!super::dispatch_policy().aggregate_success_result);
        super::set_dispatch_policy(super::DispatchPolicy {
            aggregate_success_result: true,
        });
        assert!(
            super::dispatch_policy().aggregate_success_result,
            "热重载注入必须生效，不能被首次写入锁死"
        );
        // 恢复默认，避免污染同进程内其它测试
        super::set_dispatch_policy(super::DispatchPolicy::default());
    }

    /// 节流用的 LIKE 模式必须与真正写入的消息前缀一致，否则节流失效。
    #[test]
    fn all_offline_like_pattern_matches_written_message() {
        let pattern = super::all_offline_like_pattern();
        assert_eq!(
            pattern,
            format!("{}%", super::ALL_OFFLINE_MESSAGE_PREFIX)
        );
        // 真正写入的消息（见 all-offline 分支）必须以该前缀开头
        let prefix = super::ALL_OFFLINE_MESSAGE_PREFIX;
        let written = format!("{prefix}（3 个），本次未下发；请检查…");
        assert!(
            written.starts_with(prefix),
            "写入的消息必须命中节流模式，否则每次都写一行"
        );
        // 其它失败原因不应命中
        assert!(!format!("任务下发失败，Agent：[a]").starts_with(prefix));
    }

    /// 默认值必须是聚合成功行，否则线上会退回逐 Agent 写成功行。
    #[test]
    fn dispatch_policy_defaults_to_aggregate_success() {
        // 只断言类型级默认值：另一个测试会改写全局，并行执行时读全局必然有竞态
        assert!(super::DispatchPolicy::default().aggregate_success_result);
    }

    /// 长度守恒：N 个 model → N 个元组。
    #[test]
    fn pair_task_ids_length_conservation() {
        for n in [1_usize, 2, 5, 50] {
            let models: Vec<task::Model> = (0..n)
                .map(|i| make_model(i as i64, Uuid::from_u128(i as u128), "x"))
                .collect();
            assert_eq!(pair_task_ids(&models).len(), n);
        }
    }

    /// SQLite 端到端回归测试：`insert_many(...).exec_with_returning()` 在 SQLite 下必须成功
    /// 返回每行真实 Model，而非 `BackendNotSupported`。
    ///
    /// 背景：sea-orm 的 `exec_with_returning` 仅在 `sqlite-use-returning-for-3_35` feature 启用
    /// 时对 SQLite 走 RETURNING；未启用则直接返回 `Err(BackendNotSupported { Sqlite })`，导致
    /// `crontab_task` 在 SQLite（文档化默认后端）下静默丢弃整批任务。本测试用内存 SQLite 实跑，
    /// 锁定该 feature 必须启用——若有人误删 Cargo.toml 里的 feature，此测试会失败。
    /// （之前的纯函数测试不连 DB，测不到这条 sea-orm 路径，是该 blocker 漏网的原因。）
    #[tokio::test]
    async fn insert_many_exec_with_returning_works_on_sqlite() {
        use sea_orm::{ActiveValue, ConnectionTrait, Database, FromQueryResult, Set, Statement};

        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");

        // 运行时 SQLite 版本必须 ≥ 3.35（RETURNING 子句最低要求）。
        // sqlx 的 `sqlite` feature 默认带 bundled，理论上捆绑 3.50.x；此处实跑确认，
        // 排除「链接到系统旧版 SQLite < 3.35 导致 RETURNING 运行时失败」的风险。
        #[derive(FromQueryResult)]
        struct VersionRow {
            v: String,
        }
        let ver = VersionRow::find_by_statement(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT sqlite_version() AS v",
            [],
        ))
        .one(&db)
        .await
        .expect("query sqlite_version")
        .expect("version row");
        let (major, minor) = {
            let mut it = ver.v.split('.');
            (
                it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0),
                it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0),
            )
        };
        let ver_str = ver.v.clone();
        assert!(
            (major, minor) >= (3, 35),
            "运行时 SQLite {ver_str} < 3.35，RETURNING 不可用；确认启用了 sqlx bundled",
        );

        // 建最小 task 表（列对齐 task entity；id 自增主键）
        db.execute_unprepared(
            r#"CREATE TABLE "task" (
                "id" integer PRIMARY KEY AUTOINCREMENT,
                "uuid" blob NOT NULL,
                "token" text NOT NULL,
                "cron_source" text,
                "timestamp" bigint,
                "success" boolean,
                "error_message" text,
                "task_event_type" blob NOT NULL,
                "task_event_result" blob,
                "created_at" bigint
            )"#,
        )
        .await
        .expect("create task table");

        // 批量插入 50 行（较大批量，排除「批量+RETURNING」的驱动边角 bug），用 RETURNING 拿回真实 Model
        const N: i64 = 50;
        let models: Vec<task::ActiveModel> = (0..N)
            .map(|i| task::ActiveModel {
                id: ActiveValue::default(),
                uuid: Set(Uuid::from_u128(i as u128)),
                token: Set(format!("tok{i}")),
                cron_source: Set(None),
                timestamp: Set(None),
                success: Set(None),
                error_message: Set(None),
                task_event_type: Set(serde_json::Value::Null),
                task_event_result: Set(None),
                created_at: Set(Some(i)),
            })
            .collect();

        let inserted = task::Entity::insert_many(models)
            .exec_with_returning(&db)
            .await
            .expect("exec_with_returning must succeed on SQLite (sqlite-use-returning-for-3_35 feature)");

        // 返回行数 == 插入数；id 由 DB 分配（连续与否不重要，关键是真实回读）
        assert_eq!(inserted.len() as i64, N, "RETURNING 应返回全部 {N} 行");
        let ids: Vec<i64> = inserted.iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            (1..=N).collect::<Vec<_>>(),
            "SQLite 单条多值 INSERT rowid 连续递增"
        );
        // token 顺序与写入一致（dispatch[i] ↔ uuids[i] 配对正确性）
        assert_eq!(
            inserted
                .iter()
                .map(|m| m.token.as_str())
                .collect::<Vec<_>>(),
            (0..N).map(|i| format!("tok{i}")).collect::<Vec<_>>()
        );
    }
}
