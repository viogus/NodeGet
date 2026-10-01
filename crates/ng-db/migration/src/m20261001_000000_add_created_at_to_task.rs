use sea_orm_migration::prelude::*;

/// 给 `task` 表新增 `created_at`（epoch 毫秒）列，并建立 `(uuid, created_at)` 索引。
///
/// 背景：`task` 原先只有 `timestamp`——它在任务**完成**时由
/// `upload_task_result` 回填；派发后从未上传结果的行 `timestamp IS NULL`。
/// 而既有清理链路（JS Worker `server-task-worker` 的 `clean_up_database`）生成的是
/// `timestamp <= end` 条件，NULL 永不匹配，这类行因此永久堆积（生产实测 10 万行量级）。
///
/// 保留期必须按「记录创建时间」计算，故新增本列：
/// - 新写入的行一定带值（见 `crates/ng-crontab/src/task.rs`、`ng-task/src/rpc/create_task*.rs`）；
/// - 历史行留 NULL，由清理器按 id 安全边界单独回收（见 `crates/ng-task/src/retention.rs`）。
///
/// 列可空且**不做全表回填**：`ALTER TABLE ... ADD COLUMN`（无 NOT NULL / 无 DEFAULT）在
/// PostgreSQL 是纯元数据操作（毫秒级、不重写表）；695k 行的全表 UPDATE 会长时间持锁、
/// 放大 WAL，而清理器已能自行处理 NULL 行，回填没有收益。
///
/// 索引 `(uuid, created_at)` 供清理器按 Agent 做范围删除；`(uuid, timestamp)` 仍被查询
/// 路径使用（`ng-task/src/rpc/query.rs`），不在本迁移改动。
#[derive(DeriveMigrationName)]
pub struct Migration;

/// 索引名，供迁移与文档共用。
pub const INDEX_UUID_CREATED_AT: &str = "idx-task-uuid-created-at";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(TaskInDatabase::Table)
                    .add_column(
                        ColumnDef::new(TaskInDatabase::CreatedAt)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // 与 m20260608_000000_add_indexes 一致：索引用原始 SQL 建，以同时兼容
        // PostgreSQL 与 SQLite（sea-query 的 Index::create 无 IF NOT EXISTS 语义）。
        manager
            .get_connection()
            .execute_unprepared(&format!(
                r#"CREATE INDEX IF NOT EXISTS "{INDEX_UUID_CREATED_AT}" ON "task" ("uuid", "created_at")"#
            ))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&format!(r#"DROP INDEX IF EXISTS "{INDEX_UUID_CREATED_AT}""#))
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TaskInDatabase::Table)
                    .drop_column(TaskInDatabase::CreatedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum TaskInDatabase {
    #[sea_orm(iden = "task")]
    Table,
    CreatedAt,
}
