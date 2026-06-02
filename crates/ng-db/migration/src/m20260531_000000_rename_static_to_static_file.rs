use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                // PostgreSQL: ALTER TABLE RENAME + ALTER INDEX RENAME
                manager
                    .get_connection()
                    .execute_unprepared(r#"ALTER TABLE "static" RENAME TO "static_file""#)
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"ALTER INDEX "idx-static-name-unique" RENAME TO "idx-static-file-name-unique""#,
                    )
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"ALTER INDEX "idx-static-is-http-root-unique" RENAME TO "idx-static-file-is-http-root-unique""#,
                    )
                    .await?;
            }
            sea_orm::DatabaseBackend::Sqlite => {
                // SQLite 3.25+ 支持 ALTER TABLE RENAME TO，但不支持 ALTER INDEX RENAME
                // 需要手动 DROP + CREATE 重建索引
                manager
                    .get_connection()
                    .execute_unprepared(r#"ALTER TABLE "static" RENAME TO "static_file""#)
                    .await?;

                // 重建 name 唯一索引
                manager
                    .get_connection()
                    .execute_unprepared(r#"DROP INDEX IF EXISTS "idx-static-name-unique""#)
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"CREATE UNIQUE INDEX "idx-static-file-name-unique" ON "static_file" ("name")"#,
                    )
                    .await?;

                // 重建 partial unique index (is_http_root)
                manager
                    .get_connection()
                    .execute_unprepared(r#"DROP INDEX IF EXISTS "idx-static-is-http-root-unique""#)
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"CREATE UNIQUE INDEX "idx-static-file-is-http-root-unique" ON "static_file" ("is_http_root") WHERE "is_http_root" = 1"#,
                    )
                    .await?;
            }
            _ => {
                // 其他后端不支持，直接跳过
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                manager
                    .get_connection()
                    .execute_unprepared(r#"ALTER TABLE "static_file" RENAME TO "static""#)
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"ALTER INDEX "idx-static-file-name-unique" RENAME TO "idx-static-name-unique""#,
                    )
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"ALTER INDEX "idx-static-file-is-http-root-unique" RENAME TO "idx-static-is-http-root-unique""#,
                    )
                    .await?;
            }
            sea_orm::DatabaseBackend::Sqlite => {
                manager
                    .get_connection()
                    .execute_unprepared(r#"ALTER TABLE "static_file" RENAME TO "static""#)
                    .await?;

                // 重建回旧索引名
                manager
                    .get_connection()
                    .execute_unprepared(r#"DROP INDEX IF EXISTS "idx-static-file-name-unique""#)
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"CREATE UNIQUE INDEX "idx-static-name-unique" ON "static" ("name")"#,
                    )
                    .await?;

                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"DROP INDEX IF EXISTS "idx-static-file-is-http-root-unique""#,
                    )
                    .await?;
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"CREATE UNIQUE INDEX "idx-static-is-http-root-unique" ON "static" ("is_http_root") WHERE "is_http_root" = 1"#,
                    )
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }
}
