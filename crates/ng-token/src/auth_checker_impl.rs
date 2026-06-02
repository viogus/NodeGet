//! AuthChecker 的具体实现，将 ng-infra 的同步认证接口桥接到异步 Token 查询。
//!
//! 在 server 启动时通过 `ng_token::register_auth_checker()` 注册为全局 AuthChecker。

use ng_core::permission::data_structure::Token;
use ng_core::permission::token_auth::TokenOrAuth;
use ng_infra::server::AuthChecker;

use crate::get::get_token;

/// 基于 TokenCache 的 `AuthChecker` 实现。
///
/// 在 server 启动时通过 `ng_token::register_auth_checker()` 注册。
///
/// # 安全约束
///
/// 此实现使用 `tokio::task::block_in_place` + `Handle::block_on`
/// 将同步的 `AuthChecker::check` 桥接到异步的 `get_token`。
/// **必须**仅在 tokio 多线程 runtime 内调用，
/// 在非 runtime 上下文或已有 `block_on` 内调用会导致 panic。
pub struct TokenAuthChecker;

impl AuthChecker for TokenAuthChecker {
    /// 验证原始凭据字符串并返回已认证的 Token 信息。
    ///
    /// - `raw_token`：原始凭据，支持 `key:secret` 或 `username|password` 格式
    /// - 返回：认证成功后的 Token 结构体
    ///
    /// 内部步骤：
    /// 1. 将原始字符串解析为 TokenOrAuth 枚举
    /// 2. 使用 block_in_place 在当前线程阻塞等待异步认证结果
    ///    （block_in_place 允许 runtime 在此线程阻塞时继续调度其他任务）
    fn check(&self, raw_token: &str) -> anyhow::Result<Token> {
        let token_or_auth = TokenOrAuth::from_full_token(raw_token)
            .map_err(|e| ng_core::error::NodegetError::ParseError(e.to_string()))?;

        tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(get_token(&token_or_auth))
        })
    }
}
