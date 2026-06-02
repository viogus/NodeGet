//! Token RPC 模块定义与路由分发。
//!
//! 定义 `token` 命名空间下的所有 JSON-RPC 方法：
//! - `token_get`：查询 Token 信息
//! - `token_create`：创建子令牌
//! - `token_delete`：删除令牌
//! - `token_change_password`：修改密码
//! - `token_roll_token_secret`：轮换 Token secret
//! - `token_list_all_tokens`：列出所有 Token
//! - `token_edit`：编辑令牌权限
//!
//! 每个方法通过 `rpc_exec!` 宏统一处理日志和错误转换，
//! 并使用 `token_identity` 提取认证标识用于 tracing span。

use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use ng_core::permission::create::TokenCreationRequest;
use ng_core::permission::data_structure::Limit;
use ng_infra::rpc_exec;
use ng_infra::server::token_identity;
use serde_json::value::RawValue;
use tracing::Instrument;

mod change_password;
mod create;
mod delete;
mod edit;
mod get;
mod list_all_tokens;
mod roll_token_secret;
mod utils;

/// Token RPC trait 定义，namespace 为 `token`。
///
/// 使用 `#[rpc]` 宏自动生成 server/client 侧代码，
/// namespace 分隔符为 `_`（自定义 jsonrpsee fork）。
#[rpc(server, namespace = "token")]
pub trait Rpc {
    /// 查询 Token 信息，支持超级令牌模式下的 key/username 查询。
    #[method(name = "get")]
    async fn get(&self, token: String, supertoken: Option<String>) -> RpcResult<Box<RawValue>>;

    /// 创建子令牌，需提供父级超级令牌。
    #[method(name = "create")]
    async fn create(
        &self,
        father_token: String,
        token_creation: TokenCreationRequest,
    ) -> RpcResult<Box<RawValue>>;

    /// 删除指定令牌，仅超级令牌可调用。
    #[method(name = "delete")]
    async fn delete(&self, token: String, target_token: String) -> RpcResult<Box<RawValue>>;

    /// 修改指定令牌的密码，仅超级令牌可调用。
    #[method(name = "change_password")]
    async fn change_password(
        &self,
        token: String,
        target_token: String,
        new_password: String,
    ) -> RpcResult<Box<RawValue>>;

    /// 轮换指定令牌的 secret，仅超级令牌可调用。
    #[method(name = "roll_token_secret")]
    async fn roll_token_secret(
        &self,
        token: String,
        target_token: String,
    ) -> RpcResult<Box<RawValue>>;

    /// 列出所有令牌，仅超级令牌可调用。
    #[method(name = "list_all_tokens")]
    async fn list_all_tokens(&self, token: String) -> RpcResult<Box<RawValue>>;

    /// 编辑指定令牌的权限限制，仅超级令牌可调用。
    #[method(name = "edit")]
    async fn edit(
        &self,
        token: String,
        target_token: String,
        limit: Vec<Limit>,
    ) -> RpcResult<Box<RawValue>>;
}

/// Token RPC 的空状态实现体，所有方法委托到子模块函数。
pub struct TokenRpcImpl;

/// TokenRpcImpl 的 RPC 方法实现。
///
/// 每个方法的处理模式统一：
/// 1. 使用 `token_identity` 从凭据字符串中提取 (token_key, username) 用于日志
/// 2. 创建 tracing span 附加上下文信息
/// 3. 通过 `rpc_exec!` 宏委托到对应子模块函数，统一处理日志和错误转换
#[jsonrpsee::core::async_trait]
impl RpcServer for TokenRpcImpl {
    async fn get(&self, token: String, supertoken: Option<String>) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "token", "token::get", token_key = tk, username = un, has_supertoken = supertoken.is_some());
        async { rpc_exec!(get::get(token, supertoken).await) }
            .instrument(span)
            .await
    }

    async fn create(
        &self,
        father_token: String,
        token_creation: TokenCreationRequest,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&father_token);
        let span = tracing::info_span!(target: "token", "token::create", token_key = tk, username = un, target_username = ?token_creation.username);
        async { rpc_exec!(create::create(father_token, token_creation).await) }
            .instrument(span)
            .await
    }

    async fn delete(&self, token: String, target_token: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let (target_tk, target_un) = token_identity(&target_token);
        let span = tracing::info_span!(target: "token", "token::delete", token_key = tk, username = un, target_token_key = target_tk, target_username = target_un);
        async { rpc_exec!(delete::delete(token, target_token).await) }
            .instrument(span)
            .await
    }

    async fn change_password(
        &self,
        token: String,
        target_token: String,
        new_password: String,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let (target_tk, target_un) = token_identity(&target_token);
        let span = tracing::info_span!(
            target: "token",
            "token::change_password",
            token_key = tk,
            username = un,
            target_token_key = target_tk,
            target_username = target_un,
        );
        async {
            rpc_exec!(change_password::change_password(token, target_token, new_password).await)
        }
        .instrument(span)
        .await
    }

    async fn roll_token_secret(
        &self,
        token: String,
        target_token: String,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let (target_tk, target_un) = token_identity(&target_token);
        let span = tracing::info_span!(
            target: "token",
            "token::roll_token_secret",
            token_key = tk,
            username = un,
            target_token_key = target_tk,
            target_username = target_un,
        );
        async { rpc_exec!(roll_token_secret::roll_token_secret(token, target_token).await) }
            .instrument(span)
            .await
    }

    async fn list_all_tokens(&self, token: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "token", "token::list_all_tokens", token_key = tk, username = un);
        async { rpc_exec!(list_all_tokens::list_all_tokens(token).await) }
            .instrument(span)
            .await
    }

    async fn edit(
        &self,
        token: String,
        target_token: String,
        limit: Vec<Limit>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let (target_tk, target_un) = token_identity(&target_token);
        let span = tracing::info_span!(target: "token", "token::edit", token_key = tk, username = un, target_token_key = target_tk, target_username = target_un);
        async { rpc_exec!(edit::edit(token, target_token, limit).await) }
            .instrument(span)
            .await
    }
}
