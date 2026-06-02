//! 静态文件桶（Static Bucket）权限校验模块。
//!
//! 职责：通过 OnceLock 注入的 [`TokenPermissionChecker`] trait 实现，
//! 对 `static-bucket` 和 `static-bucket-file` 两个 RPC 命名空间
//! 的调用方进行 Scope + Permission 级别的细粒度鉴权。
//!
//! 协作关系：服务器二进制在启动时调用 [`set_token_checker`] 注入具体实现，
//! 各 RPC handler 在执行业务逻辑前调用 [`check_static_bucket_permission`]
//! 或 [`check_static_bucket_file_permission`] 完成鉴权。

use ng_core::error::NodegetError;
use ng_core::permission::data_structure::{
    Permission, Scope, StaticBucket as StaticBucketPermission,
    StaticBucketFile as StaticBucketFilePermission,
};
use ng_core::permission::token_auth::TokenOrAuth;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use tracing::{trace, warn};

// ── TokenPermissionChecker trait + 全局注入 ─────────────────────────────

/// 静态文件桶权限校验所需的 Token 权限检查 trait。
///
/// 服务器 crate 必须实现此 trait，并在启动时通过 [`set_token_checker`] 注入。
pub trait TokenPermissionChecker: Send + Sync {
    /// 检查 Token/Auth 是否满足给定的 Scope 和 Permission 约束。
    fn check_token_limit(
        &self,
        token_or_auth: &TokenOrAuth,
        scopes: Vec<Scope>,
        permissions: Vec<Permission>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>>;

    /// 检查 Token/Auth 是否为 SuperToken。
    fn check_super_token(
        &self,
        token_or_auth: &TokenOrAuth,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>>;
}

/// 全局 TokenPermissionChecker 单例，服务器启动时注入。
static TOKEN_CHECKER: OnceLock<Box<dyn TokenPermissionChecker>> = OnceLock::new();

/// 注入全局 TokenPermissionChecker 实现。
///
/// 必须在服务器启动阶段调用且仅调用一次。
pub fn set_token_checker(checker: Box<dyn TokenPermissionChecker>) {
    let _ = TOKEN_CHECKER.set(checker);
}

/// 获取全局 TokenPermissionChecker 实例。
///
/// 若未初始化则 panic——必须在启动时先调用 [`set_token_checker`]。
pub fn get_token_checker() -> &'static dyn TokenPermissionChecker {
    TOKEN_CHECKER
        .get()
        .expect("TokenPermissionChecker not initialized -- call set_token_checker first")
        .as_ref()
}

// ── 静态文件桶权限检查 ──────────────────────────────────────────────

/// 校验指定 Token 是否拥有对某个静态文件桶的特定操作权限。
///
/// - `token` - 完整的 Token 字符串（key:secret 或 username|password 格式）
/// - `name` - 目标静态文件桶名称，同时作为 Scope 的标识
/// - `permission` - 需要校验的 [`StaticBucketPermission`] 操作类型
///
/// 返回：权限通过返回 `Ok(())`，否则返回 `PermissionDenied` 错误。
///
/// 内部步骤：
/// 1. 将 token 字符串解析为 [`TokenOrAuth`]
/// 2. 以 `Scope::StaticBucket(name)` + `Permission::StaticBucket(permission)` 调用全局 checker
/// 3. 校验失败时记录 warn 日志并返回错误
pub async fn check_static_bucket_permission(
    token: &str,
    name: &str,
    permission: StaticBucketPermission,
) -> anyhow::Result<()> {
    trace!(target: "static_bucket", name = %name, permission = ?permission, "checking static-bucket permission");
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let permission_name = format!("{permission:?}");
    let is_allowed = get_token_checker()
        .check_token_limit(
            &token_or_auth,
            vec![Scope::StaticBucket(name.to_owned())],
            vec![Permission::StaticBucket(permission)],
        )
        .await?;

    if is_allowed {
        return Ok(());
    }

    warn!(target: "static_bucket", name = %name, permission = %permission_name, "permission denied");
    Err(NodegetError::PermissionDenied(format!(
        "Permission denied for static-bucket '{name}', required permission: {permission_name}"
    ))
    .into())
}

/// 校验指定 Token 是否拥有对某个静态文件桶内文件的特定操作权限。
///
/// - `token` - 完整的 Token 字符串（key:secret 或 username|password 格式）
/// - `name` - 目标静态文件桶名称，同时作为 Scope 的标识
/// - `permission` - 需要校验的 [`StaticBucketFilePermission`] 操作类型
///
/// 返回：权限通过返回 `Ok(())`，否则返回 `PermissionDenied` 错误。
///
/// 内部步骤：
/// 1. 将 token 字符串解析为 [`TokenOrAuth`]
/// 2. 以 `Scope::StaticBucket(name)` + `Permission::StaticBucketFile(permission)` 调用全局 checker
/// 3. 校验失败时记录 warn 日志并返回错误
pub async fn check_static_bucket_file_permission(
    token: &str,
    name: &str,
    permission: StaticBucketFilePermission,
) -> anyhow::Result<()> {
    trace!(target: "static_bucket_file", name = %name, permission = ?permission, "checking static-bucket-file permission");
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let permission_name = format!("{permission:?}");
    let is_allowed = get_token_checker()
        .check_token_limit(
            &token_or_auth,
            vec![Scope::StaticBucket(name.to_owned())],
            vec![Permission::StaticBucketFile(permission)],
        )
        .await?;

    if is_allowed {
        return Ok(());
    }

    warn!(target: "static_bucket_file", name = %name, permission = %permission_name, "permission denied");
    Err(NodegetError::PermissionDenied(format!(
        "Permission denied for static-bucket-file '{name}', required permission: {permission_name}"
    ))
    .into())
}
