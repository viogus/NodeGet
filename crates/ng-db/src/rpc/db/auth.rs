//! `db` 命名空间 RPC 的认证与名称校验
//!
//! 提供 `Token` 权限检查和数据库名称合法性校验，供 `create`/`read`/`update`/`delete`/`exec_sql` 复用。

use crate::rpc::auth_provider;
use ng_core::error::NodegetError;
use ng_core::permission::data_structure::{Db as DbPermission, Permission, Scope};
use ng_core::permission::token_auth::TokenOrAuth;

/// 检查 Token 对指定数据库是否具有所需权限
///
/// - `token` — 原始 token 字符串（`key:secret` 或 `username|password` 格式）
/// - `db_name` — 目标数据库名称，构造 `Scope::Db(db_name)` 作用域
/// - `permission` — 所需的数据库权限级别（Create/Read/Update/Delete/ExecSql）
/// - 返回值：权限通过返回 `Ok(())`，否则返回 `PermissionDenied` 错误
///
/// # Errors
///
/// 当 Token 解析失败、认证提供者未初始化、权限检查失败或权限不足时返回错误
pub async fn check_db_permission(
    token: &str,
    db_name: &str,
    permission: DbPermission,
) -> anyhow::Result<()> {
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let provider = auth_provider()
        .ok_or_else(|| NodegetError::Other("Auth provider not initialized".to_owned()))?;

    let is_allowed = provider
        .check_token_limit(
            &token_or_auth,
            vec![Scope::Db(db_name.to_owned())],
            vec![Permission::Db(permission.clone())],
        )
        .await?;

    if !is_allowed {
        tracing::warn!(target: "db", db_name = db_name, "db permission denied for Db::{permission:?} on {db_name}");
        return Err(NodegetError::PermissionDenied(format!(
            "Permission Denied: Requires Db::{permission:?} on Scope::Db({db_name})"
        ))
        .into());
    }

    Ok(())
}

/// 校验数据库名称合法性
///
/// - `name` — 待校验的名称
/// - 返回值：合法返回 `Ok(())`，否则返回 `InvalidInput` 错误
///
/// 校验规则：
/// 1. 非空
/// 2. 长度不超过 128 字符
/// 3. 仅允许 ASCII 字母、数字、下划线、连字符、点号
/// 4. 不允许 `.` 或 `..`（防止路径遍历）
///
/// # Errors
///
/// 当名称为空、超长、包含非法字符或为路径遍历模式时返回 `InvalidInput` 错误
pub fn validate_db_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        return Err(NodegetError::InvalidInput("db name cannot be empty".to_owned()).into());
    }
    if name.len() > 128 {
        return Err(
            NodegetError::InvalidInput("db name too long (max 128 chars)".to_owned()).into(),
        );
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    if !valid {
        return Err(NodegetError::InvalidInput(
            "db name contains invalid characters (only [A-Za-z0-9_.-] allowed)".to_owned(),
        )
        .into());
    }
    if name == "." || name == ".." {
        return Err(NodegetError::InvalidInput("db name cannot be '.' or '..'".to_owned()).into());
    }
    Ok(())
}
