use crate::sea_orm::DbBackend;
use sea_orm_migration::prelude::*;

/// 对「高删除量」的三张普通表做细粒度 autovacuum 调参，抑制死元组堆积与表膨胀。
///
/// 依据（arm 生产库实测）：`task` / `crontab_result` / `js_result` 由保留期清理器
/// （`crates/ng-task/src/retention.rs`）按小时批量删除，删除量在 10^4–10^5 行/天量级。
/// PostgreSQL 默认 `autovacuum_vacuum_scale_factor = 0.2` 意味着 54 万行的
/// `crontab_result` 要攒到约 10.8 万死元组才触发一次清理，期间堆与索引持续膨胀；
/// 而 `autovacuum_vacuum_threshold = 50` 对这几张表又过于宽松。
///
/// 故把这三张表调成：
/// - `autovacuum_vacuum_scale_factor = 0.05`（约 5% 死元组即触发，54 万行时约 2.7 万行）
/// - `autovacuum_vacuum_threshold = 500`
///
/// 只动 vacuum 阈值，**不动** `autovacuum_vacuum_cost_*`：该实例磁盘 I/O 已接近饱和
/// （日志里有 `slow statement ... INSERT INTO "dynamic_monitoring"`），不做无节流清理。
/// 统计信息一侧无需调参：清理器每轮开头都会 `ANALYZE` 这三张表
/// （见 `retention.rs` 的 `refresh_statistics`）。
///
/// `ALTER TABLE ... SET (...)` 是纯元数据操作（毫秒级、不重写表），且只有 PostgreSQL 支持，
/// 故按后端分支；SQLite 直接跳过（其 vacuum 由 `auto_vacuum` pragma 控制，与这里无关）。
#[derive(DeriveMigrationName)]
pub struct Migration;

/// 参与调参的表：都由保留期清理器按时间批量删除。
const TABLES: [&str; 3] = ["task", "crontab_result", "js_result"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DbBackend::Postgres => {}
            _ => return Ok(()),
        }

        for table in TABLES {
            manager
                .get_connection()
                .execute_unprepared(&format!(
                    r#"ALTER TABLE "{table}" SET (autovacuum_vacuum_scale_factor = 0.05, autovacuum_vacuum_threshold = 500)"#
                ))
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DbBackend::Postgres => {}
            _ => return Ok(()),
        }

        for table in TABLES {
            manager
                .get_connection()
                .execute_unprepared(&format!(
                    r#"ALTER TABLE "{table}" RESET (autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold)"#
                ))
                .await?;
        }

        Ok(())
    }
}
