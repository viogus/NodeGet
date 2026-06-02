//! `token_change_password` RPC 方法实现。
//!
//! 修改指定 Token 的密码，仅超级令牌可调用。

use jsonrpsee::core::RpcResult;
use ng_core::error::NodegetError;
use ng_db::entity::token;
use sea_orm::{ActiveModelTrait, Set};
use serde_json::value::RawValue;
use tracing::debug;

use super::utils::{find_target_token, verify_supertoken};
use crate::cache::TokenCache;
use crate::hash_string;

/// 修改指定 Token 的密码（仅超级令牌可调用）。
///
/// - `token`：超级令牌凭据（用于鉴权）
/// - `target_token`：目标 Token 的 `token_key` 或 `username`；
///   支持纯 `token_key`、`username`，或 `token_key:secret` 格式（secret 不校验）
/// - `new_password`：新密码，非空且不少于 6 个字符
/// - 返回：成功时为 `{"success":true,"message":"Password changed successfully"}`
/// - 错误：鉴权失败、目标不存在、数据库错误
///
/// 内部步骤：
/// 1. 验证调用者为超级令牌
/// 2. 校验新密码非空且长度 >= 6
/// 3. 查找目标 Token 记录
/// 4. 计算新密码哈希，更新数据库
/// 5. 刷新 TokenCache
pub async fn change_password(
    token: String,
    target_token: String,
    new_password: String,
) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        verify_supertoken(&token).await?;

        if new_password.is_empty() {
            return Err(
                NodegetError::InvalidInput("New password cannot be empty".to_owned()).into(),
            );
        }
        if new_password.len() < 6 {
            return Err(NodegetError::InvalidInput(
                "New password must be at least 6 characters long".to_owned(),
            )
            .into());
        }

        let target_model = find_target_token(&target_token).await?;

        let new_password_hash = hash_string(&new_password);

        let db = ng_db::get_db().ok_or_else(|| {
            NodegetError::DatabaseError("Database connection not initialized".to_owned())
        })?;

        let mut active_model: token::ActiveModel = target_model.into();
        active_model.password_hash = Set(Some(new_password_hash));

        let updated = active_model
            .update(db)
            .await
            .map_err(|e| NodegetError::DatabaseError(format!("Failed to update password: {e}")))?;

        debug!(
            target: "token",
            token_key = %updated.token_key,
            username = ?updated.username,
            "password changed successfully"
        );

        // 更新成功后刷新缓存
        if let Err(e) = TokenCache::reload().await {
            tracing::error!(
                target: "token",
                error = %e,
                "Failed to reload token cache after password change"
            );
        }

        let response = r#"{"success":true,"message":"Password changed successfully"}"#.to_owned();
        RawValue::from_string(response)
            .map_err(|e| NodegetError::SerializationError(format!("{e}")).into())
    };

    // 统一错误转换：anyhow → NodegetError → JSON-RPC ErrorObject
    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => {
            let nodeget_err = ng_core::error::anyhow_to_nodeget_error(&e);
            Err(jsonrpsee::types::ErrorObject::owned(
                nodeget_err.error_code() as i32,
                format!("{nodeget_err}"),
                None::<()>,
            ))
        }
    }
}
