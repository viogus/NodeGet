//! Terminal 权限校验模块。
//!
//! 职责：通过 OnceLock 注入的 [`TokenPermissionChecker`] trait 实现，
//! 对 Terminal WebSocket 连接进行 Scope + Permission 级别的鉴权。
//!
//! 协作关系：服务器二进制在启动时调用 [`set_token_checker`] 注入具体实现，
//! [`check_terminal_connect_permission`] 在 User 连接 WebSocket 前被调用。
//!
//! 权限模型：先检查 `AgentUuid` Scope 的 `Terminal::Connect` 权限，
//! 若不通过再回退检查 `Global` Scope，两者任一通过即可连接。

use ng_core::error::NodegetError;
use ng_core::permission::data_structure::{Permission, Scope, Terminal};
use ng_core::permission::token_auth::TokenOrAuth;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use tracing::trace;
use uuid::Uuid;

// ── TokenPermissionChecker trait + 全局注入 ─────────────────────────────

/// Terminal 权限校验所需的 Token 权限检查 trait。
///
/// 服务器 crate 必须实现此 trait，并在启动时通过 [`set_token_checker`] 注入。
pub trait TokenPermissionChecker: Send + Sync + 'static {
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
        .expect("TokenPermissionChecker not initialized — call set_token_checker first")
        .as_ref()
}

// ── Terminal 连接权限检查 ─────────────────────────────────────────────

/// 校验指定 Token 是否拥有连接到目标 Agent Terminal 的权限。
///
/// - `token` - 完整的 Token 字符串（key:secret 或 username|password 格式）
/// - `agent_uuid` - 目标 Agent 的 UUID 字符串
///
/// 返回：权限通过返回 `Ok(())`，否则返回 `PermissionDenied` 错误。
///
/// 内部步骤：
/// 1. 解析 agent_uuid 为 [`Uuid`]，格式不合法时返回 ParseError
/// 2. 将 token 解析为 [`TokenOrAuth`]
/// 3. 先检查 `AgentUuid` Scope 下的 `Terminal::Connect` 权限
/// 4. 若不通过，回退检查 `Global` Scope 下的 `Terminal::Connect` 权限
/// 5. 两者均不通过时返回 PermissionDenied 错误
pub async fn check_terminal_connect_permission(
    token: &str,
    agent_uuid: &str,
) -> anyhow::Result<()> {
    trace!(target: "terminal", agent_uuid = %agent_uuid, "checking terminal connect permission");
    let agent_uuid = Uuid::parse_str(agent_uuid)
        .map_err(|_| NodegetError::ParseError("Invalid Agent UUID format".to_owned()))?;

    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let checker = get_token_checker();

    // 先检查 AgentUuid Scope 的权限
    let has_agent_permission = checker
        .check_token_limit(
            &token_or_auth,
            vec![Scope::AgentUuid(agent_uuid)],
            vec![Permission::Terminal(Terminal::Connect)],
        )
        .await?;

    if has_agent_permission {
        return Ok(());
    }

    // 回退检查 Global Scope 的权限
    let has_global_permission = checker
        .check_token_limit(
            &token_or_auth,
            vec![Scope::Global],
            vec![Permission::Terminal(Terminal::Connect)],
        )
        .await?;

    if has_global_permission {
        return Ok(());
    }

    Err(NodegetError::PermissionDenied(format!(
        "No terminal connect permission for agent '{agent_uuid}'"
    ))
    .into())
}
