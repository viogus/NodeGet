use crate::db_registry::{
    DbExecResult, DbRegistryManager, is_read_query, json_to_sea_value, row_to_json,
};
use crate::rpc::db::auth::check_db_permission;
use crate::rpc::{to_rpc_error, token_identity};
use jsonrpsee::core::RpcResult;
use ng_core::error::NodegetError;
use ng_core::permission::data_structure::Db as DbPermission;
use sea_orm::ConnectionTrait;
use serde_json::value::RawValue;
use tracing::debug;

/// Core SQL execution logic for `exec_sql`.
///
/// Performs permission check, parameter validation, query execution, and result serialization.
pub(crate) async fn exec_sql_inner(
    db_name: &str,
    sql: &str,
    params: Option<serde_json::Value>,
    token: &str,
) -> anyhow::Result<Box<RawValue>> {
    check_db_permission(token, db_name, DbPermission::ExecSql).await?;

    let mgr = DbRegistryManager::global();
    let db_conn = mgr
        .get_conn(db_name)
        .await
        .ok_or_else(|| NodegetError::DatabaseError(format!("Database '{db_name}' not found")))?;

    let sea_params = match params {
        Some(serde_json::Value::Array(arr)) => arr.iter().map(json_to_sea_value).collect(),
        Some(serde_json::Value::Null) | None => vec![],
        _ => {
            return Err(
                NodegetError::InvalidInput("params must be an array or null".to_owned()).into(),
            );
        }
    };

    let db_backend = db_conn.get_database_backend();
    let stmt = sea_orm::Statement::from_sql_and_values(db_backend, sql, sea_params);

    let is_select = is_read_query(sql);

    let result = if is_select {
        let rows = db_conn.query_all_raw(stmt).await?;
        let json_rows: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
        let rc = json_rows.len() as u64;
        DbExecResult {
            success: true,
            data: json_rows,
            row_count: rc,
        }
    } else {
        let exec_result = db_conn.execute_raw(stmt).await?;
        DbExecResult {
            success: true,
            data: vec![],
            row_count: exec_result.rows_affected(),
        }
    };

    let (tk, un) = token_identity(token);
    debug!(target: "db", token_key = tk, username = un, name = %db_name, sql_len = sql.len(), "exec_sql");

    let resp = serde_json::json!({
        "success": result.success,
        "data": result.data,
        "row_count": result.row_count,
    });

    let json_str = serde_json::to_string(&resp)?;
    RawValue::from_string(json_str)
        .map_err(|e| NodegetError::SerializationError(e.to_string()).into())
}

pub async fn exec_sql(
    token: String,
    name: String,
    sql: String,
    params: Option<serde_json::Value>,
) -> RpcResult<Box<RawValue>> {
    match exec_sql_inner(&name, &sql, params, &token).await {
        Ok(result) => Ok(result),
        Err(e) => Err(to_rpc_error(&e)),
    }
}
