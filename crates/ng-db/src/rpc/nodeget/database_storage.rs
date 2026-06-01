use crate::rpc::to_rpc_error;
use ng_core::error::NodegetError;
use ng_core::permission::token_auth::TokenOrAuth;
use sea_orm::{DatabaseBackend, DatabaseConnection, FromQueryResult, Statement};
use serde::Serialize;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use tracing::debug;

/// 排除的系统表名前缀/模式
const EXCLUDED_TABLES: &[&str] = &["seaql_migrations"];

#[derive(FromQueryResult)]
struct TableSizeRow {
    table_name: String,
    table_size: i64,
}

#[derive(Serialize)]
struct DatabaseStorageResponse {
    /// 各表存储占用（字节）
    tables: BTreeMap<String, i64>,
    /// 数据库总大小（字节），所有表之和
    total: i64,
}

pub async fn database_storage(token: String) -> jsonrpsee::core::RpcResult<Box<RawValue>> {
    debug!(target: "server", "querying database storage");
    let process_logic = async {
        // 验证 super token 权限
        let token_or_auth = TokenOrAuth::from_full_token(&token)
            .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

        let provider = crate::rpc::auth_provider()
            .ok_or_else(|| NodegetError::Other("Auth provider not initialized".to_owned()))?;

        let is_super = provider
            .check_super_token(&token_or_auth)
            .await
            .map_err(|e| NodegetError::PermissionDenied(format!("{e}")))?;

        if !is_super {
            return Err(NodegetError::PermissionDenied(
                "Permission Denied: Super token required".to_owned(),
            )
            .into());
        }
        debug!(target: "server", "Super token verified for database_storage");

        let db = crate::get_db().ok_or_else(|| {
            ng_core::error::NodegetError::DatabaseError("DB not initialized".to_owned())
        })?;
        let tables = match db.get_database_backend() {
            DatabaseBackend::Postgres => query_postgres(db).await?,
            DatabaseBackend::Sqlite => query_sqlite(db).await?,
            backend => {
                return Err(NodegetError::Other(format!(
                    "Unsupported database backend: {backend:?}"
                ))
                .into());
            }
        };

        let total: i64 = tables.values().sum();
        let response = DatabaseStorageResponse { tables, total };
        debug!(target: "server", total_bytes = total, "Database storage query completed");

        let json_str = serde_json::to_string(&response)
            .map_err(|e| NodegetError::SerializationError(e.to_string()))?;

        RawValue::from_string(json_str)
            .map_err(|e| NodegetError::SerializationError(e.to_string()).into())
    };

    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => Err(to_rpc_error(&e)),
    }
}

/// `PostgreSQL`: 动态发现用户表并查询各表总大小（含索引和 TOAST）
async fn query_postgres(db: &DatabaseConnection) -> anyhow::Result<BTreeMap<String, i64>> {
    debug!(target: "server", "querying postgres table sizes");
    // 从 information_schema 动态获取当前 schema 下的所有用户表
    let discover_sql = r"
        SELECT tablename AS table_name
        FROM pg_tables
        WHERE schemaname = current_schema()
          AND tablename NOT IN (SELECT unnest($1::text[]))
        ORDER BY tablename
    ";
    let excluded: Vec<String> = EXCLUDED_TABLES.iter().map(ToString::to_string).collect();
    let table_names: Vec<String> = TableNameRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        discover_sql,
        [excluded.into()],
    ))
    .all(db)
    .await
    .map_err(|e| NodegetError::DatabaseError(e.to_string()))?
    .into_iter()
    .map(|r| r.table_name)
    .collect();

    if table_names.is_empty() {
        return Ok(BTreeMap::new());
    }

    // 使用 unnest 将表名数组展开，一次查询获取所有表的大小
    let size_sql = r"
        SELECT
            t.name AS table_name,
            COALESCE(pg_total_relation_size(t.name::regclass), 0) AS table_size
        FROM unnest($1::text[]) AS t(name)
        ORDER BY t.name
    ";
    let rows = TableSizeRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        size_sql,
        [table_names.into()],
    ))
    .all(db)
    .await
    .map_err(|e| NodegetError::DatabaseError(e.to_string()))?;

    let mut result = BTreeMap::new();
    for row in rows {
        result.insert(row.table_name, row.table_size);
    }
    debug!(target: "server", table_count = result.len(), "Postgres table sizes queried");

    Ok(result)
}

#[derive(FromQueryResult)]
struct SizeRow {
    table_size: i64,
}

#[derive(FromQueryResult)]
struct TableNameRow {
    table_name: String,
}

/// `SQLite`: 动态发现用户表并使用 dbstat 虚拟表查询各表占用的页面总大小
async fn query_sqlite(db: &DatabaseConnection) -> anyhow::Result<BTreeMap<String, i64>> {
    debug!(target: "server", "querying sqlite table sizes");
    // 从 sqlite_master 动态获取所有用户表
    let excluded: Vec<String> = EXCLUDED_TABLES.iter().map(ToString::to_string).collect();
    let placeholders: Vec<String> = excluded.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let not_in_clause = if placeholders.is_empty() {
        String::new()
    } else {
        format!(" AND name NOT IN ({})", placeholders.join(", "))
    };
    let discover_sql = format!(
        "SELECT name AS table_name FROM sqlite_master WHERE type = 'table'{not_in_clause} ORDER BY name"
    );
    let values: Vec<sea_orm::Value> = excluded.into_iter().map(|s| s.into()).collect();
    let table_names: Vec<String> = TableNameRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        &discover_sql,
        values,
    ))
    .all(db)
    .await
    .map_err(|e| NodegetError::DatabaseError(e.to_string()))?
    .into_iter()
    .map(|r| r.table_name)
    .collect();

    let mut result = BTreeMap::new();
    for table_name in &table_names {
        // dbstat 虚拟表在 SQLite 编译时需启用 SQLITE_ENABLE_DBSTAT_VTAB
        // sqlx 的 bundled SQLite 默认启用此选项
        let sql = "SELECT COALESCE(SUM(pgsize), 0) AS table_size FROM dbstat WHERE name = ?";

        let row = SizeRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            sql,
            [table_name.as_str().into()],
        ))
        .one(db)
        .await
        .map_err(|e| NodegetError::DatabaseError(e.to_string()))?;

        let size = row.map_or(0, |r| r.table_size);
        result.insert(table_name.clone(), size);
    }
    debug!(target: "server", table_count = result.len(), "SQLite table sizes queried");

    Ok(result)
}
