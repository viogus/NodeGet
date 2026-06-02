//! ng-token：NodeGet 的 Token 管理核心 crate
//!
//! 负责令牌的生成、验证、缓存与权限校验，是 RBAC 体系的实现层。
//!
//! ## 默认 feature（仅类型）
//! 从 ng-core 重新导出 Token、Limit、Scope、Permission、TokenOrAuth 等类型，
//! 使 Agent 和其他 crate 可以仅依赖类型定义而不引入 server 逻辑。
//!
//! ## `server` feature
//! - `TokenCache` — 基于 DB 的内存令牌缓存，使用 DbBackedCache + make_global_cache! 模式
//! - `super_token` — 超级令牌的生成、轮换与验证（常量时间比较）
//! - `generate_token` — 子令牌生成与存储
//! - `get` — 令牌查询、权限匹配与校验
//! - RPC namespace — `token_*` JSON-RPC 方法（namespace 分隔符为 `_`）
//! - `AuthChecker` impl — 与 ng-infra 全局认证注入集成

// ── 从 ng-core 重新导出的类型（始终可用）──────────────────────────

pub use ng_core::error::{NodegetError, anyhow_to_nodeget_error};
pub use ng_core::permission::data_structure::{Limit, Permission, Scope, Token};
pub use ng_core::permission::token_auth::TokenOrAuth;

// ── 仅 server feature 启用的模块 ──────────────────────────────────

#[cfg(feature = "server")]
pub mod cache;
#[cfg(feature = "server")]
pub mod generate_token;
#[cfg(feature = "server")]
pub mod get;
#[cfg(feature = "server")]
pub mod rpc;
#[cfg(feature = "server")]
pub mod super_token;

// ── 仅 server feature 启用的重新导出 ──────────────────────────────

#[cfg(feature = "server")]
pub use cache::TokenCache;
#[cfg(feature = "server")]
pub use get::{
    check_token_limit, get_token, get_token_by_key_or_username, parse_token_limit_with_compat,
};
#[cfg(feature = "server")]
pub use super_token::check_super_token;

#[cfg(feature = "server")]
mod auth_checker_impl;

// ── 共享哈希工具（仅 server，被多个模块使用）────────────────────────

#[cfg(feature = "server")]
use sha2::{Digest, Sha256};

/// 使用 NODEGET 盐值对字符串进行 SHA256 哈希，返回十六进制编码结果。
///
/// - `need_hash`：待哈希的原始字符串（如 token_secret 或 password）
/// - 返回：64 字符的十六进制字符串
#[cfg(feature = "server")]
pub fn hash_string(need_hash: &str) -> String {
    let bytes = hash_to_bytes(need_hash);
    hex::encode(bytes)
}

/// 使用 NODEGET 盐值对字符串进行 SHA256 哈希，返回原始 32 字节摘要。
///
/// 用于常量时间比较场景（如 Token 认证），避免十六进制字符串的额外解码开销。
///
/// - `need_hash`：待哈希的原始字符串
/// - 返回：`[u8; 32]` 原始摘要，可直接与 `ct_eq` 比较
#[cfg(feature = "server")]
pub fn hash_to_bytes(need_hash: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"NODEGET"); // 盐值，与 token_hash/password_hash 存储格式一致
    hasher.update(need_hash.as_bytes());
    hasher.finalize().into()
}

// ── AuthChecker 集成入口 ──────────────────────────────────────────

/// 将本 crate 的 `AuthChecker` 实现注册到 ng-infra 的全局注入点。
///
/// 必须在 server 启动期间、`TokenCache::init()` 之后调用一次。
/// 注册后，所有通过 `ng_infra::server::auth_check` 发起的认证请求
/// 都会委托给 `TokenAuthChecker`。
#[cfg(feature = "server")]
pub fn register_auth_checker() {
    ng_infra::server::set_auth_checker(Box::new(auth_checker_impl::TokenAuthChecker));
}

/// 构建并返回 token RPC 模块。
///
/// 调用方应在启动时将此模块合并到主 RPC 模块：
/// ```ignore
/// main_module.merge(ng_token::rpc_module()).unwrap();
/// ```
#[cfg(feature = "server")]
pub fn rpc_module() -> jsonrpsee::RpcModule<rpc::TokenRpcImpl> {
    use rpc::RpcServer;
    rpc::TokenRpcImpl.into_rpc()
}
