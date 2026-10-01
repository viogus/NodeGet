//! 服务端数据库保留期清理：`task` / `crontab_result` / `js_result`。
//!
//! ## 为什么要在服务端做
//!
//! 这三张表的清理原先完全由 JS Worker（`server-task-worker` 的 `clean_up_database`，
//! 每天 00:00 触发）负责，实际运行中有三处缺陷：
//!
//! 1. **`task` 的僵尸行永不清理**：JS 侧按 `timestamp <= end` 删除，而 `timestamp` 只在
//!    任务**完成**时由结果上传回填；派发后从未回传结果的行 `timestamp IS NULL`，
//!    永不满足 `<=` 条件，于是无限堆积（生产库实测 10 万行量级）。
//! 2. **软删除 Agent 的数据永不清理**：JS 侧把软删除 Agent 的保留期置 0（意图是全删），
//!    但 `if (duration)` 把 0 当假值跳过，这些 Agent 的全部历史行因此永久保留。
//! 3. **只遍历当前 Agent 列表**：列表之外的 UUID（Agent 被硬删除后留下的行）不会被清理。
//!
//! 另外 JS Worker 的调度是可被覆盖的（`base-worker` 的 `upsertCrons()` 会按名字重写
//! crontab），清理这种「必须发生」的事情不该依赖它。
//!
//! ## 保留期真源
//!
//! 仍然复用 `kv` 表中 UI 可改的 `database_limit_*` 键，不引入新的配置来源：
//! - Agent 级：命名空间为该 Agent 的 UUID 字符串；
//! - 全局级：命名空间 `global`；
//! - Agent 级优先，缺失时回退全局级；两者都没有 → **跳过该 Agent**（保守，宁可不删）。
//!
//! `crontab_result` 与 `js_result` 没有 uuid 列（见 `ng_db::entity`），只按全局级保留期清理。
//!
//! ## 其他语义
//!
//! - `task` 的年龄以 `created_at`（派发时间，epoch 毫秒）为准；`created_at IS NULL` 的历史行
//!   （本功能上线前写入，无法知道创建时间）按 **id 安全边界**回收：只删除
//!   `id <= max(id) - legacy_id_margin` 的行，默认 100_000 行（约数小时写入量），
//!   以免误删刚刚派发、结果还在回传途中的任务。
//! - 软删除 Agent（`monitoring_uuid.soft_delete = true`）的保留期视为 0，但保留
//!   `SOFT_DELETE_GRACE_MS`（60 秒）宽限，避免删掉仍在途的任务行。
//! - 时间列本身为 NULL 的 `crontab_result` / `js_result` 行不会被清理：没有可用的时间
//!   基准，删除它们可能误伤刚写入的记录。
//! - 每轮清扫开始前对三张目标表执行 `ANALYZE`，避免新增列缺少统计信息时规划器退化成
//!   全表扫描（详见 `refresh_statistics`）。
//!
//! ## 运行方式
//!
//! [`spawn_worker`] 起一个后台循环，默认每 [`DEFAULT_INTERVAL_SECS`] 秒执行一次。
//! **默认是 dry-run**（`RetentionOptions::enabled = false`）：只统计并打日志、不删除任何行，
//! 便于先核对将要删除的数量；确认后再由配置打开真正删除。
//! 启动行与每轮汇总行统一用 `warn!`（dry-run 与真删都一样）：生产默认 `log_filter = "warn"`，
//! INFO 级看不见——真删是否在跑、这一轮删了多少，都属于运维必须看到的信息。

use std::collections::HashMap;
use std::time::Duration;

use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use tracing::{debug, error, warn};
use uuid::Uuid;

use ng_db::entity::{crontab_result, js_result, kv, monitoring_uuid, task};

/// `kv` 中保留期键的前缀，完整键形如 `database_limit_task`。
const LIMIT_KEY_PREFIX: &str = "database_limit_";

/// 全局保留期所在的 `kv` 命名空间。
const GLOBAL_NAMESPACE: &str = "global";

/// 单次删除分块大小。取 500 是为了兼容 SQLite 的绑定变量上限（默认 999）。
const CHUNK_SIZE: u64 = 500;

/// 单个清理目标的单次上限分块数（500 × 2000 = 100 万行），防止异常数据把一次清理拖成无界循环。
const MAX_CHUNKS_PER_TARGET: u32 = 2_000;

/// 软删除 Agent 的清理宽限（毫秒），保护仍在回传途中的任务行。
const SOFT_DELETE_GRACE_MS: i64 = 60_000;

/// 默认清理间隔：1 小时。
pub const DEFAULT_INTERVAL_SECS: u64 = 3_600;

/// 默认首次清理延迟：30 秒，避开服务启动高峰。
pub const DEFAULT_INITIAL_DELAY_SECS: u64 = 30;

/// 历史行（`created_at IS NULL`）默认的 id 安全边界。
pub const DEFAULT_LEGACY_ID_MARGIN: i64 = 100_000;

/// 清理器行为选项。
#[derive(Debug, Clone, Copy)]
pub struct RetentionOptions {
    /// 是否真正删除。`false`（默认）= dry-run：只统计并打日志。
    pub enabled: bool,
    /// 两次清理的间隔。
    pub interval: Duration,
    /// 启动后首次清理的延迟。
    pub initial_delay: Duration,
    /// 历史行的 id 安全边界，见模块文档。
    pub legacy_id_margin: i64,
}

impl Default for RetentionOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            initial_delay: Duration::from_secs(DEFAULT_INITIAL_DELAY_SECS),
            legacy_id_margin: DEFAULT_LEGACY_ID_MARGIN,
        }
    }
}

/// 一次清理的统计结果。
#[derive(Debug, Default, Clone, Copy)]
pub struct SweepReport {
    /// 活跃 Agent：按 Agent 保留期删除的 `task` 行数。
    pub task_active: u64,
    /// 软删除 Agent：整行清除的 `task` 行数（保留期视为 0）。
    pub task_soft_deleted: u64,
    /// 历史行（`created_at IS NULL`）按 id 边界回收的 `task` 行数。
    pub task_legacy: u64,
    /// 已不在 `monitoring_uuid` 中的孤立 `task` 行数。
    pub task_orphan: u64,
    /// 因既无 Agent 级也无全局级 `database_limit_task` 而跳过的 Agent 数。
    pub agents_without_limit: usize,
    /// `crontab_result` 删除行数（按全局保留期，时间基准 `run_time`）。
    pub crontab_result: u64,
    /// `js_result` 删除行数（按全局保留期，时间基准 `start_time`）。
    pub js_result: u64,
}

impl SweepReport {
    /// 本次清理涉及的全部行数。
    #[must_use]
    pub fn total(&self) -> u64 {
        self.task_active
            + self.task_soft_deleted
            + self.task_legacy
            + self.task_orphan
            + self.crontab_result
            + self.js_result
    }
}

/// 刷新清理目标表的统计信息。
///
/// PostgreSQL 的规划器依赖统计信息挑索引；`task.created_at` 这类**新增列**在首次清扫时
/// 没有任何统计，规划器会因此低估选择性、退化成按主键的全表索引扫描（生产实测单次查询
/// 8.45 秒、`shared hit=367916`；`ANALYZE` 后同一查询 3 个 buffer、0.073 毫秒）。
/// 每轮清扫前刷新一次代价很小（采样分析），却能让分块分页稳定走索引。
///
/// 失败只记 `debug!`：统计信息不属于正确性，刷新不了也不该中断清理。
async fn refresh_statistics(db: &DatabaseConnection) {
    for table in ["task", "crontab_result", "js_result"] {
        if let Err(err) = db.execute_unprepared(&format!("ANALYZE {table}")).await {
            debug!(
                target: "retention",
                table,
                %err,
                "analyze failed, keeping existing statistics"
            );
        }
    }
}

/// 执行一次清理。
///
/// `dry_run = true` 时只统计、不删除。`legacy_id_margin` 见模块文档。
///
/// # Errors
///
/// 任一数据库操作失败时返回 [`DbErr`]；调用方（[`spawn_worker`]）会记录错误并在下一个
/// 周期重试，不会中断服务。
pub async fn sweep_once(
    db: &DatabaseConnection,
    dry_run: bool,
    legacy_id_margin: i64,
) -> Result<SweepReport, DbErr> {
    let now_ms = crate::now_millis();
    refresh_statistics(db).await;
    let limits = load_limits(db).await?;
    let global_task = limit_for(&limits, GLOBAL_NAMESPACE, "task");
    let mut report = SweepReport::default();

    let agents = monitoring_uuid::Entity::find().all(db).await?;
    debug!(
        target: "retention",
        agent_count = agents.len(),
        dry_run,
        "retention sweep started"
    );

    // 1. 逐 Agent 清理 task（Agent 级保留期优先，回退全局级）
    for agent in &agents {
        let uuid = agent.uuid;
        let filter = if agent.soft_delete {
            Condition::all()
                .add(task::Column::Uuid.eq(uuid))
                .add(task::Column::CreatedAt.is_not_null())
                .add(task::Column::CreatedAt.lt(now_ms.saturating_sub(SOFT_DELETE_GRACE_MS)))
        } else {
            let Some(limit_ms) = limit_for(&limits, &uuid.to_string(), "task").or(global_task)
            else {
                report.agents_without_limit += 1;
                continue;
            };
            Condition::all()
                .add(task::Column::Uuid.eq(uuid))
                .add(task::Column::CreatedAt.is_not_null())
                .add(task::Column::CreatedAt.lt(now_ms.saturating_sub(limit_ms)))
        };

        let deleted = purge::<task::Entity>(db, task::Column::Id, filter, dry_run).await?;
        if agent.soft_delete {
            report.task_soft_deleted += deleted;
        } else {
            report.task_active += deleted;
        }
    }

    // 2. 历史行：迁移前写入、created_at 为 NULL，用 id 安全边界回收
    let max_id: Option<i64> = task::Entity::find()
        .select_only()
        .column(task::Column::Id)
        .order_by_desc(task::Column::Id)
        .limit(1)
        .into_tuple()
        .one(db)
        .await?;
    if let Some(max_id) = max_id {
        let watermark = max_id.saturating_sub(legacy_id_margin.max(0));
        let filter = Condition::all()
            .add(task::Column::CreatedAt.is_null())
            .add(task::Column::Id.lte(watermark));
        report.task_legacy = purge::<task::Entity>(db, task::Column::Id, filter, dry_run).await?;
    }

    // 3. 孤立行：uuid 已不在 monitoring_uuid 中（Agent 被硬删除），按全局保留期回收
    if let Some(global_limit_ms) = global_task
        && !agents.is_empty()
    {
        let uuids: Vec<Uuid> = agents.iter().map(|agent| agent.uuid).collect();
        let filter = Condition::all()
            .add(task::Column::CreatedAt.is_not_null())
            .add(task::Column::CreatedAt.lt(now_ms.saturating_sub(global_limit_ms)))
            .add(task::Column::Uuid.is_not_in(uuids));
        report.task_orphan = purge::<task::Entity>(db, task::Column::Id, filter, dry_run).await?;
    }

    // 4. crontab_result：无 uuid 列，只按全局保留期（时间基准 run_time）
    if let Some(limit_ms) = limit_for(&limits, GLOBAL_NAMESPACE, "crontab_result") {
        let filter = Condition::all()
            .add(crontab_result::Column::RunTime.is_not_null())
            .add(crontab_result::Column::RunTime.lt(now_ms.saturating_sub(limit_ms)));
        report.crontab_result =
            purge::<crontab_result::Entity>(db, crontab_result::Column::Id, filter, dry_run)
                .await?;
    }

    // 5. js_result：无 uuid 列，只按全局保留期（时间基准 start_time）
    if let Some(limit_ms) = limit_for(&limits, GLOBAL_NAMESPACE, "js_result") {
        let filter = Condition::all()
            .add(js_result::Column::StartTime.is_not_null())
            .add(js_result::Column::StartTime.lt(now_ms.saturating_sub(limit_ms)));
        report.js_result =
            purge::<js_result::Entity>(db, js_result::Column::Id, filter, dry_run).await?;
    }

    Ok(report)
}

/// 起一个后台清理循环：延迟 [`RetentionOptions::initial_delay`] 后清理一次，
/// 之后每 [`RetentionOptions::interval`] 重复。
///
/// dry-run（`enabled = false`）与真删模式的启动行、每轮汇总行都用 `warn!` 输出，
/// 保证在生产 `log_filter = "warn"` 下都可见。
pub fn spawn_worker(db: &'static DatabaseConnection, options: RetentionOptions) {
    let dry_run = !options.enabled;
    if dry_run {
        warn!(
            target: "retention",
            interval_secs = options.interval.as_secs(),
            initial_delay_secs = options.initial_delay.as_secs(),
            legacy_id_margin = options.legacy_id_margin,
            "retention sweeper started in DRY-RUN mode: nothing will be deleted; \
             set `retention.enabled = true` in config to actually purge"
        );
    } else {
        warn!(
            target: "retention",
            interval_secs = options.interval.as_secs(),
            initial_delay_secs = options.initial_delay.as_secs(),
            legacy_id_margin = options.legacy_id_margin,
            "retention sweeper started in ENFORCE mode: expired rows will be deleted"
        );
    }

    tokio::spawn(async move {
        tokio::time::sleep(options.initial_delay).await;
        loop {
            match sweep_once(db, dry_run, options.legacy_id_margin).await {
                Ok(report) if dry_run => warn!(
                    target: "retention",
                    dry_run = true,
                    task_active = report.task_active,
                    task_soft_deleted = report.task_soft_deleted,
                    task_legacy = report.task_legacy,
                    task_orphan = report.task_orphan,
                    agents_without_limit = report.agents_without_limit,
                    crontab_result = report.crontab_result,
                    js_result = report.js_result,
                    total = report.total(),
                    "retention sweep (DRY-RUN) finished: these rows would be deleted"
                ),
                Ok(report) => warn!(
                    target: "retention",
                    dry_run = false,
                    task_active = report.task_active,
                    task_soft_deleted = report.task_soft_deleted,
                    task_legacy = report.task_legacy,
                    task_orphan = report.task_orphan,
                    agents_without_limit = report.agents_without_limit,
                    crontab_result = report.crontab_result,
                    js_result = report.js_result,
                    total = report.total(),
                    "retention sweep finished: rows deleted"
                ),
                Err(e) => error!(
                    target: "retention",
                    error = %e,
                    "retention sweep failed, will retry next interval"
                ),
            }
            tokio::time::sleep(options.interval).await;
        }
    });
}

/// 读取全部 `database_limit_*` 配置，键为 `(namespace, key)`。
async fn load_limits(db: &DatabaseConnection) -> Result<HashMap<(String, String), i64>, DbErr> {
    let keys: Vec<String> = ["task", "crontab_result", "js_result"]
        .iter()
        .map(|table| format!("{LIMIT_KEY_PREFIX}{table}"))
        .collect();

    let rows = kv::Entity::find()
        .filter(kv::Column::Key.is_in(keys))
        .all(db)
        .await?;

    let mut limits = HashMap::with_capacity(rows.len());
    for row in rows {
        match parse_limit_ms(&row.value) {
            Some(ms) => {
                limits.insert((row.namespace, row.key), ms);
            }
            None => warn!(
                target: "retention",
                namespace = %row.namespace,
                key = %row.key,
                value = %row.value,
                "ignoring unparsable database limit value"
            ),
        }
    }
    Ok(limits)
}

/// 取 `(namespace, database_limit_<table>)` 的保留期。
fn limit_for(limits: &HashMap<(String, String), i64>, namespace: &str, table: &str) -> Option<i64> {
    limits
        .get(&(namespace.to_owned(), format!("{LIMIT_KEY_PREFIX}{table}")))
        .copied()
}

/// 解析保留期配置值（毫秒）。容忍 JSON 数字与数字字符串；负数视为非法。
fn parse_limit_ms(value: &serde_json::Value) -> Option<i64> {
    let ms = match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }?;
    (ms >= 0).then_some(ms)
}

/// 按主键游标分块统计或删除，返回涉及行数。
///
/// 分块的目的：首次运行时待删行数可达十万级，一次性 `DELETE` 会产生长事务与 WAL 尖峰；
/// 分块后每块 500 行、独立提交。游标同时让 dry-run 能完整计数（不删除时不会自然推进）。
async fn purge<E>(
    db: &DatabaseConnection,
    pk: E::Column,
    filter: Condition,
    dry_run: bool,
) -> Result<u64, DbErr>
where
    E: EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    let mut total: u64 = 0;
    let mut cursor: Option<i64> = None;

    for _ in 0..MAX_CHUNKS_PER_TARGET {
        let mut query = E::find().select_only().column(pk).filter(filter.clone());
        if let Some(cursor) = cursor {
            query = query.filter(pk.gt(cursor));
        }
        let ids: Vec<i64> = query
            .order_by_asc(pk)
            .limit(CHUNK_SIZE)
            .into_tuple()
            .all(db)
            .await?;

        let Some(last) = ids.last().copied() else {
            break;
        };
        let chunk_len = u64::try_from(ids.len()).unwrap_or(0);
        total += chunk_len;

        if !dry_run {
            E::delete_many().filter(pk.is_in(ids)).exec(db).await?;
        }

        if chunk_len < CHUNK_SIZE {
            break;
        }
        cursor = Some(last);
    }

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::{RetentionOptions, sweep_once};
    use ng_db::entity::{crontab_result, js_result, kv, monitoring_uuid, task};
    use sea_orm::{ActiveValue, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Set};
    use uuid::Uuid;

    const DAY_MS: i64 = 86_400_000;
    const HOUR_MS: i64 = 3_600_000;
    const MINUTE_MS: i64 = 60_000;

    /// 建最小 schema（列对齐各 entity），供端到端清理测试使用。
    async fn setup_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");

        for ddl in [
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
            r#"CREATE TABLE "crontab_result" (
                "id" integer PRIMARY KEY AUTOINCREMENT,
                "cron_id" bigint NOT NULL,
                "cron_name" text NOT NULL,
                "relative_id" bigint,
                "run_time" bigint,
                "success" boolean,
                "message" text
            )"#,
            r#"CREATE TABLE "js_result" (
                "id" integer PRIMARY KEY AUTOINCREMENT,
                "js_worker_id" bigint NOT NULL,
                "js_worker_name" text NOT NULL,
                "run_type" text NOT NULL,
                "start_time" bigint,
                "finish_time" bigint,
                "param" blob,
                "result" blob,
                "error_message" text
            )"#,
            r#"CREATE TABLE "kv" (
                "id" integer PRIMARY KEY AUTOINCREMENT,
                "namespace" text NOT NULL,
                "key" text NOT NULL,
                "value" blob NOT NULL
            )"#,
            r#"CREATE TABLE "monitoring_uuid" (
                "id" integer PRIMARY KEY AUTOINCREMENT,
                "uuid" blob NOT NULL,
                "soft_delete" boolean NOT NULL
            )"#,
        ] {
            db.execute_unprepared(ddl).await.expect("create schema");
        }

        db
    }

    async fn insert_agent(db: &DatabaseConnection, uuid: Uuid, soft_delete: bool) {
        monitoring_uuid::Entity::insert(monitoring_uuid::ActiveModel {
            id: ActiveValue::default(),
            uuid: Set(uuid),
            soft_delete: Set(soft_delete),
        })
        .exec(db)
        .await
        .expect("insert monitoring_uuid");
    }

    async fn insert_limit(db: &DatabaseConnection, namespace: &str, key: &str, ms: i64) {
        kv::Entity::insert(kv::ActiveModel {
            id: ActiveValue::default(),
            namespace: Set(namespace.to_owned()),
            key: Set(key.to_owned()),
            value: Set(serde_json::json!(ms)),
        })
        .exec(db)
        .await
        .expect("insert kv");
    }

    async fn insert_task(db: &DatabaseConnection, uuid: Uuid, created_at: Option<i64>) {
        task::Entity::insert(task::ActiveModel {
            id: ActiveValue::default(),
            uuid: Set(uuid),
            token: Set("token".to_owned()),
            cron_source: Set(None),
            timestamp: Set(None),
            success: Set(None),
            error_message: Set(None),
            task_event_type: Set(serde_json::Value::Null),
            task_event_result: Set(None),
            created_at: Set(created_at),
        })
        .exec(db)
        .await
        .expect("insert task");
    }

    async fn insert_crontab_result(db: &DatabaseConnection, run_time: Option<i64>) {
        crontab_result::Entity::insert(crontab_result::ActiveModel {
            id: ActiveValue::default(),
            cron_id: Set(1),
            cron_name: Set("cron".to_owned()),
            relative_id: Set(None),
            run_time: Set(run_time),
            success: Set(Some(true)),
            message: Set(None),
        })
        .exec(db)
        .await
        .expect("insert crontab_result");
    }

    async fn insert_js_result(db: &DatabaseConnection, start_time: Option<i64>) {
        js_result::Entity::insert(js_result::ActiveModel {
            id: ActiveValue::default(),
            js_worker_id: Set(1),
            js_worker_name: Set("worker".to_owned()),
            run_type: Set("cron".to_owned()),
            start_time: Set(start_time),
            finish_time: Set(None),
            param: Set(None),
            result: Set(None),
            error_message: Set(None),
        })
        .exec(db)
        .await
        .expect("insert js_result");
    }

    async fn count<E>(db: &DatabaseConnection) -> usize
    where
        E: EntityTrait,
    {
        E::find().all(db).await.expect("count rows").len()
    }

    /// 默认配置必须是 dry-run：没有显式打开时绝不允许删除生产数据。
    #[test]
    fn default_options_are_dry_run() {
        let options = RetentionOptions::default();
        assert!(!options.enabled, "默认必须只统计不删除");
        assert_eq!(options.interval.as_secs(), super::DEFAULT_INTERVAL_SECS);
        assert_eq!(
            options.initial_delay.as_secs(),
            super::DEFAULT_INITIAL_DELAY_SECS
        );
        assert_eq!(options.legacy_id_margin, super::DEFAULT_LEGACY_ID_MARGIN);
    }

    /// 端到端：dry-run 只统计、真跑才删除，且第二次清理为空操作。
    #[tokio::test]
    async fn sweep_counts_then_deletes_expired_rows() {
        let db = setup_db().await;
        let now = crate::now_millis();
        let active = Uuid::from_u128(1);
        let deleted = Uuid::from_u128(2);
        let orphan = Uuid::from_u128(3);

        insert_agent(&db, active, false).await;
        insert_agent(&db, deleted, true).await;

        // 全局 task 保留 1 天；active 的 Agent 级保留 1 小时
        insert_limit(&db, "global", "database_limit_task", DAY_MS).await;
        insert_limit(&db, &active.to_string(), "database_limit_task", HOUR_MS).await;
        insert_limit(&db, "global", "database_limit_crontab_result", DAY_MS).await;
        insert_limit(&db, "global", "database_limit_js_result", DAY_MS).await;

        // task：过期 / 未过期 / 历史行(NULL) / 软删除过期 / 软删除宽限内 / 孤儿过期 / 孤儿未过期
        insert_task(&db, active, Some(now - 2 * HOUR_MS)).await;
        insert_task(&db, active, Some(now - 10 * MINUTE_MS)).await;
        insert_task(&db, active, None).await;
        insert_task(&db, deleted, Some(now - 5 * MINUTE_MS)).await;
        insert_task(&db, deleted, Some(now - 10_000)).await;
        insert_task(&db, orphan, Some(now - 2 * DAY_MS)).await;
        insert_task(&db, orphan, Some(now - 10 * MINUTE_MS)).await;

        // 无 uuid 列的两张表：过期 / 未过期 / 时间列为 NULL（必须保留）
        insert_crontab_result(&db, Some(now - 2 * DAY_MS)).await;
        insert_crontab_result(&db, Some(now - 10 * MINUTE_MS)).await;
        insert_crontab_result(&db, None).await;
        insert_js_result(&db, Some(now - 2 * DAY_MS)).await;
        insert_js_result(&db, Some(now - 10 * MINUTE_MS)).await;
        insert_js_result(&db, None).await;

        let tasks_before = count::<task::Entity>(&db).await;

        // dry-run：数量可预测，但一行都不许删
        let report = sweep_once(&db, true, 0).await.expect("dry-run sweep");
        assert_eq!(report.task_active, 1);
        assert_eq!(report.task_soft_deleted, 1);
        assert_eq!(report.task_legacy, 1);
        assert_eq!(report.task_orphan, 1);
        assert_eq!(report.agents_without_limit, 0);
        assert_eq!(report.crontab_result, 1);
        assert_eq!(report.js_result, 1);
        assert_eq!(report.total(), 6);
        assert_eq!(
            count::<task::Entity>(&db).await,
            tasks_before,
            "dry-run 不得删除任何行"
        );

        // 真跑：同样的数量，且真正落库删除
        let report = sweep_once(&db, false, 0).await.expect("real sweep");
        assert_eq!(report.total(), 6);
        assert_eq!(count::<task::Entity>(&db).await, tasks_before - 4);
        assert_eq!(count::<crontab_result::Entity>(&db).await, 2);
        assert_eq!(count::<js_result::Entity>(&db).await, 2);

        // 幂等：再跑一次没有可删的行
        let report = sweep_once(&db, false, 0).await.expect("second sweep");
        assert_eq!(report.total(), 0, "第二次清理应为空操作");
    }

    /// 历史行按 id 水位回收：margin 保护最近若干行，避免误删刚派发、结果仍在回传途中的任务。
    #[tokio::test]
    async fn legacy_rows_respect_id_margin() {
        let db = setup_db().await;
        let agent = Uuid::from_u128(1);
        insert_agent(&db, agent, false).await;
        insert_limit(&db, "global", "database_limit_task", DAY_MS).await;
        for _ in 0..3 {
            insert_task(&db, agent, None).await;
        }

        // margin 1 → 水位 = max_id(3) - 1 = 2，只回收 id 1、2
        let report = sweep_once(&db, true, 1).await.expect("dry-run sweep");
        assert_eq!(report.task_legacy, 2);
        assert_eq!(report.task_active, 0);
        assert_eq!(count::<task::Entity>(&db).await, 3, "dry-run 不删除");

        let report = sweep_once(&db, false, 1).await.expect("real sweep");
        assert_eq!(report.task_legacy, 2);

        let remaining = task::Entity::find().all(&db).await.expect("read remaining");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, 3, "最近一行受水位保护");
    }

    /// 既无 Agent 级也无全局级保留期时保守跳过，并计入 `agents_without_limit`。
    #[tokio::test]
    async fn agent_without_any_limit_is_skipped() {
        let db = setup_db().await;
        let agent = Uuid::from_u128(9);
        insert_agent(&db, agent, false).await;
        insert_task(&db, agent, Some(crate::now_millis() - 30 * DAY_MS)).await;

        let report = sweep_once(&db, false, 0).await.expect("sweep");
        assert_eq!(report.agents_without_limit, 1);
        assert_eq!(report.task_active, 0);
        assert_eq!(
            count::<task::Entity>(&db).await,
            1,
            "没有保留期配置时不得删除任何行"
        );
    }
}
