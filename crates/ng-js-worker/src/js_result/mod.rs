//! `js-result` RPC 命名空间 —— JS 执行结果的查询与删除。
//!
//! 提供两个 RPC 方法：
//! - `js-result_query` —— 按条件查询执行结果
//! - `js-result_delete` —— 按条件删除执行结果

use jsonrpsee::core::RpcResult;
use jsonrpsee::core::async_trait;
use jsonrpsee::proc_macros::rpc;
use ng_core::js_result::query::JsResultDataQuery;
use ng_infra::rpc_exec;
use ng_infra::server::{RpcHelper, token_identity};
use serde_json::value::RawValue;
use tracing::Instrument;

mod delete;
mod query;

/// `js-result` RPC trait 定义。
#[rpc(server, namespace = "js-result")]
pub trait Rpc {
    /// 按条件查询 JS 执行结果。
    #[method(name = "query")]
    async fn query(&self, token: String, query: JsResultDataQuery) -> RpcResult<Box<RawValue>>;

    /// 按条件删除 JS 执行结果。
    #[method(name = "delete")]
    async fn delete(&self, token: String, query: JsResultDataQuery) -> RpcResult<Box<RawValue>>;
}

/// `js-result` RPC 实现。
pub struct JsResultRpcImpl;

impl RpcHelper for JsResultRpcImpl {}

#[async_trait]
impl RpcServer for JsResultRpcImpl {
    async fn query(&self, token: String, query: JsResultDataQuery) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "js_result", "js-result::query", token_key = tk, username = un, query = ?query);
        async { rpc_exec!(query::query(token, query).await) }
            .instrument(span)
            .await
    }

    async fn delete(&self, token: String, query: JsResultDataQuery) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "js_result", "js-result::delete", token_key = tk, username = un, query = ?query);
        async { rpc_exec!(delete::delete(token, query).await) }
            .instrument(span)
            .await
    }
}

/// 构建并返回 `js-result` 命名空间的 RPC Module。
pub fn rpc_module() -> jsonrpsee::RpcModule<JsResultRpcImpl> {
    JsResultRpcImpl.into_rpc()
}
