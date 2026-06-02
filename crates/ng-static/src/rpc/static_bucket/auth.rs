//! `static-bucket` 命名空间的 SuperToken 校验。
//!
//! `list` RPC 要求 SuperToken 权限，此处封装校验逻辑。

use crate::auth::get_token_checker;
use ng_core::error::NodegetError;
use ng_core::permission::token_auth::TokenOrAuth;

/// 检查给定 token 是否为 SuperToken。
///
/// - `token` - 完整的 Token 字符串
///
/// 返回：SuperToken 返回 `Ok(true)`，否则 `Ok(false)`；解析失败返回错误。
pub async fn check_super_token(token: &str) -> anyhow::Result<bool> {
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;
    get_token_checker()
        .check_super_token(&token_or_auth)
        .await
        .map_err(|e| NodegetError::PermissionDenied(format!("{e}")).into())
}
