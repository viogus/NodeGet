use crate::rpc::auth_provider;
use ng_core::error::NodegetError;
use ng_core::permission::data_structure::{Db as DbPermission, Permission, Scope};
use ng_core::permission::token_auth::TokenOrAuth;

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
