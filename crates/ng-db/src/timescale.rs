//! `TimescaleDB` 时序优化初始化。
//!
//! `NodeGet` 的数据主体是每秒一条的监控时序数据（`dynamic_monitoring`、
//! `dynamic_monitoring_summary`）。当主库是安装了 `timescaledb` 扩展的
//! `PostgreSQL` 时，本模块把这些时序表转换为 `hypertable`，并按 `chunk`
//! 配置列式压缩与自动保留策略：
//!
//! - **压缩**：对 `compress_after_days` 天前的 `chunk` 启用列式压缩（`zstd`），
//!   监控 JSON 数据通常可压缩 10 倍以上；
//! - **保留**：当 `retention_days > 0` 时，超过该天数的 `chunk` 由后台 job
//!   自动删除（`drop_chunks`），磁盘占用从此有上界；默认 `0` = 不启用，
//!   避免误删历史数据（如从普通 `PostgreSQL` 迁移过来的存量）。
//!
//! 每次服务启动时由 [`crate::init_db_connection`] 调用，全部操作幂等：
//! - 未安装 `timescaledb` 扩展 → 直接跳过（普通 `PostgreSQL` / `SQLite`
//!   不受影响）；
//! - 已是 `hypertable` / 已启用压缩 → 跳过对应转换步骤；
//! - 压缩与保留策略每次启动按配置重新应用（先移除再注册），保证配置变更
//!   在下次启动时生效。
//!
//! # 为什么不在 migration 里做
//!
//! `create_hypertable(..., migrate_data => true)` 不能在事务块内执行，
//! 而 `SeaORM` migration 默认在事务中运行。因此本模块放在 `Migrator::up`
//! 之后的非事务初始化阶段，与 `SQLite` `PRAGMA` 优化属于同一层。

use ng_core::config::TimescaleConfig;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use tracing::{debug, error, info};

/// 需要转换为 `hypertable` 的时序表。
struct TimescaleTable {
    /// 表名
    name: &'static str,
    /// 默认主键约束名（`<table>_pkey`，由 `SeaORM` 建表时生成）
    pk_name: &'static str,
    /// 压缩时的 `segmentby` 列（按 agent 分片，同一 agent 的连续行压在一起）
    segment_by: &'static str,
}

/// 一天的毫秒数，`chunk` 间隔与策略时长按整数时间列（毫秒 epoch）计算。
const MS_PER_DAY: u64 = 86_400_000;

/// 毫秒 epoch 的 `now()` 函数名，供 `set_integer_now_func` 注册。
const NOW_FUNC: &str = "ng_epoch_ms_now";

/// 创建毫秒 epoch `now()` 函数（幂等）。
/// `STABLE`：事务内返回值一致，满足 `TimescaleDB` 对 now 函数的要求。
const NOW_FUNC_SQL: &str = r"
CREATE OR REPLACE FUNCTION ng_epoch_ms_now() RETURNS BIGINT
LANGUAGE SQL STABLE PARALLEL SAFE
AS $$ SELECT (extract(epoch FROM now()) * 1000)::BIGINT $$;
";

/// 参与时序优化的表清单。
const TIMESCALE_TABLES: &[TimescaleTable] = &[
    TimescaleTable {
        name: "dynamic_monitoring",
        pk_name: "dynamic_monitoring_pkey",
        segment_by: "uuid_id",
    },
    TimescaleTable {
        name: "dynamic_monitoring_summary",
        pk_name: "dynamic_monitoring_summary_pkey",
        segment_by: "uuid_id",
    },
];

/// 初始化 `TimescaleDB` 时序优化（幂等）。
///
/// 仅在 `PostgreSQL` 且安装 `timescaledb` 扩展时执行；否则静默跳过，
/// 保证普通部署不受任何影响。
///
/// # Errors
///
/// 当扩展检测或 now 函数创建失败时返回错误（阻断启动，提示环境问题）；
/// 单表转换/策略失败仅记录 error 日志并继续，不影响服务启动。
pub async fn setup_timescale_if_available(
    db: &DatabaseConnection,
    config: Option<&TimescaleConfig>,
) -> anyhow::Result<()> {
    if db.get_database_backend() != DatabaseBackend::Postgres {
        return Ok(());
    }
    if !has_timescale_extension(db).await? {
        debug!(target: "db", "timescaledb extension not installed; skipping timescale setup");
        return Ok(());
    }

    let config = config.cloned().unwrap_or_default();
    info!(
        target: "db",
        chunk_interval_days = config.chunk_interval_days,
        compress_after_days = config.compress_after_days,
        retention_days = config.retention_days,
        "timescaledb detected; applying hypertable setup"
    );

    db.execute_unprepared(NOW_FUNC_SQL).await?;

    let mut failures = 0;
    for table in TIMESCALE_TABLES {
        if let Err(e) = setup_table(db, table, &config).await {
            failures += 1;
            error!(target: "db", table = table.name, error = %e, "timescale setup failed for table");
        }
    }
    if failures > 0 {
        error!(
            target: "db",
            failed = failures,
            total = TIMESCALE_TABLES.len(),
            "timescale setup completed with failures"
        );
    } else {
        info!(target: "db", "timescale setup completed");
    }
    Ok(())
}

/// 对单个表执行幂等的 `hypertable` 转换与策略配置。
async fn setup_table(
    db: &DatabaseConnection,
    table: &TimescaleTable,
    config: &TimescaleConfig,
) -> anyhow::Result<()> {
    if !is_hypertable(db, table.name).await? {
        // hypertable 要求所有唯一索引（含主键）包含分区列 timestamp，
        // 因此先把主键从 (id) 调整为 (id, timestamp)。
        // id 仍为自增 identity 且全局唯一，SeaORM 的按 id 删除不受影响。
        db.execute_unprepared(&format!(
            "ALTER TABLE {name} DROP CONSTRAINT IF EXISTS {pk}; \
             ALTER TABLE {name} ADD PRIMARY KEY (id, timestamp);",
            name = table.name,
            pk = table.pk_name,
        ))
        .await?;

        // migrate_data => true：把表内已有数据迁入 hypertable。
        // 必须在非事务上下文执行，见模块文档。
        db.execute_unprepared(&format!(
            "SELECT create_hypertable('{name}', 'timestamp', \
             chunk_time_interval => {chunk_ms}, migrate_data => true);",
            name = table.name,
            chunk_ms = config.chunk_interval_days * MS_PER_DAY,
        ))
        .await?;
        info!(target: "db", table = table.name, "converted to hypertable");
    }

    // 注册整数时间 now() 函数（幂等：已设置则跳过；重复注册会报错）。
    if !has_integer_now_func(db, table.name).await? {
        db.execute_unprepared(&format!(
            "SELECT set_integer_now_func('{name}', '{now_func}');",
            name = table.name,
            now_func = NOW_FUNC,
        ))
        .await?;
        info!(target: "db", table = table.name, now_func = NOW_FUNC, "integer_now_func registered");
    }

    // 压缩设置（仅首次开启；开启后 segmentby/orderby 不可随意变更）。
    if !is_compression_enabled(db, table.name).await? {
        db.execute_unprepared(&format!(
            "ALTER TABLE {name} SET (timescaledb.compress, \
             timescaledb.compress_segmentby = '{seg}', \
             timescaledb.compress_orderby = 'timestamp DESC');",
            name = table.name,
            seg = table.segment_by,
        ))
        .await?;
        info!(target: "db", table = table.name, "compression enabled");
    }

    // 压缩/保留策略：先移除再注册，使配置变更在下次启动时生效（幂等）。
    db.execute_unprepared(&format!(
        "SELECT remove_compression_policy('{name}', if_exists => true); \
         SELECT add_compression_policy('{name}', compress_after => {ms});",
        name = table.name,
        ms = config.compress_after_days * MS_PER_DAY,
    ))
    .await?;
    info!(target: "db", table = table.name, compress_after_days = config.compress_after_days, "compression policy applied");

    if config.retention_days > 0 {
        db.execute_unprepared(&format!(
            "SELECT remove_retention_policy('{name}', if_exists => true); \
             SELECT add_retention_policy('{name}', drop_after => {ms});",
            name = table.name,
            ms = config.retention_days * MS_PER_DAY,
        ))
        .await?;
        info!(target: "db", table = table.name, retention_days = config.retention_days, "retention policy applied");
    } else {
        // 显式关闭：确保不留残留策略（例如从 >0 改回 0）。
        db.execute_unprepared(&format!(
            "SELECT remove_retention_policy('{name}', if_exists => true);",
            name = table.name,
        ))
        .await?;
        debug!(target: "db", table = table.name, "retention disabled (retention_days = 0)");
    }

    Ok(())
}

/// 是否已安装 `timescaledb` 扩展。
async fn has_timescale_extension(db: &DatabaseConnection) -> anyhow::Result<bool> {
    let row = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT 1 FROM pg_extension WHERE extname = 'timescaledb' LIMIT 1".to_owned(),
        ))
        .await?;
    Ok(row.is_some())
}

/// 表是否已是 `hypertable`。
async fn is_hypertable(db: &DatabaseConnection, table: &str) -> anyhow::Result<bool> {
    let row = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT 1 FROM timescaledb_information.hypertables \
                 WHERE hypertable_name = '{table}' LIMIT 1"
            ),
        ))
        .await?;
    Ok(row.is_some())
}

/// 表是否已启用压缩。
async fn is_compression_enabled(db: &DatabaseConnection, table: &str) -> anyhow::Result<bool> {
    let row = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT 1 FROM timescaledb_information.hypertables \
                 WHERE hypertable_name = '{table}' AND compression_enabled LIMIT 1"
            ),
        ))
        .await?;
    Ok(row.is_some())
}

/// 时间维度是否已注册 `integer_now_func`。
async fn has_integer_now_func(db: &DatabaseConnection, table: &str) -> anyhow::Result<bool> {
    let row = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT 1 FROM timescaledb_information.dimensions \
                 WHERE hypertable_name = '{table}' AND integer_now_func IS NOT NULL LIMIT 1"
            ),
        ))
        .await?;
    Ok(row.is_some())
}
