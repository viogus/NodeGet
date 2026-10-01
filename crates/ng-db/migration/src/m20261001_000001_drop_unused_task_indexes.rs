use crate::sea_orm::DbBackend;
use sea_orm_migration::prelude::*;

/// 删除 4 个在生产上几乎从不被使用的索引，换取更低的写入放大与磁盘占用。
///
/// 依据（arm 生产库实测，`pg_stat_user_indexes`）：
///
/// | 索引 | 大小 | idx_scan |
/// | --- | --- | --- |
/// | `idx-task-cron-source` (task.cron_source) | 11 MB | 4 |
/// | `idx-task-task_event_type` (task.task_event_type, GIN) | 7.9 MB | 42 |
/// | `idx-crontab_result-cron_name` | 8.2 MB | 1 |
/// | `idx-crontab_result-cron_id` | 8.1 MB | 3 |
///
/// 对比同期仍在使用的索引：`task_pkey` 24,335,152 次、`idx-task-uuid-timestamp` 13,909 次、
/// `idx-crontab_result-run_time` 529 次。被删的 4 个合计约 35 MB，且 `task` 每天新增
/// 数十万行、`crontab_result` 每天新增数万行，每个索引都要同步维护 —— 低选择性列
/// （`cron_source` 只有几种取值、`cron_id`/`cron_name` 只有约 10 个取值）本就不该走索引，
/// 顺序扫描更快，这正是它们长期零使用的原因。
///
/// 使用 `DROP INDEX`（非 `CONCURRENTLY`）：迁移在事务中执行，且服务启动时是唯一的
/// 写入来源，取 ACCESS EXCLUSIVE 锁的时间是毫秒级。该语句在 PostgreSQL 与 SQLite 上
/// 都合法且幂等（与 `m20260708_000000_drop_redundant_indexes` 一致）。
#[derive(DeriveMigrationName)]
pub struct Migration;

const IDX_TASK_CRON_SOURCE: &str = "idx-task-cron-source";
const IDX_TASK_TASK_EVENT_TYPE: &str = "idx-task-task_event_type";
const IDX_CRONTAB_RESULT_CRON_NAME: &str = "idx-crontab_result-cron_name";
const IDX_CRONTAB_RESULT_CRON_ID: &str = "idx-crontab_result-cron_id";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for name in [
            IDX_TASK_CRON_SOURCE,
            IDX_TASK_TASK_EVENT_TYPE,
            IDX_CRONTAB_RESULT_CRON_NAME,
            IDX_CRONTAB_RESULT_CRON_ID,
        ] {
            manager
                .get_connection()
                .execute_unprepared(&format!(r#"DROP INDEX IF EXISTS "{name}""#))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE INDEX IF NOT EXISTS "idx-task-cron-source" ON "task" ("cron_source")"#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE INDEX IF NOT EXISTS "idx-crontab_result-cron_name" ON "crontab_result" ("cron_name")"#,
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE INDEX IF NOT EXISTS "idx-crontab_result-cron_id" ON "crontab_result" ("cron_id")"#,
            )
            .await?;

        // 与 m20260608_000000_add_indexes 保持一致：PostgreSQL 用 GIN，SQLite 用普通索引
        match manager.get_database_backend() {
            DbBackend::Postgres => {
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"CREATE INDEX IF NOT EXISTS "idx-task-task_event_type" ON "task" USING GIN ("task_event_type")"#,
                    )
                    .await?;
            }
            DbBackend::Sqlite => {
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"CREATE INDEX IF NOT EXISTS "idx-task-task_event_type" ON "task" ("task_event_type")"#,
                    )
                    .await?;
            }
            _ => {}
        }

        Ok(())
    }
}
