//! `crontab_result` RPC 命名空间实现：定时任务执行结果的查询与删除。
//!
//! 使用 jsonrpsee `#[rpc]` 宏定义 trait，`CrontabResultRpcImpl` 实现各方法，
//! 每个方法通过 `rpc_exec!` 宏统一日志与错误处理，
//! 具体业务逻辑委托到子模块（query、delete）。

use crate::query::CrontabResultDataQuery;
use jsonrpsee::core::RpcResult;
use jsonrpsee::core::async_trait;
use jsonrpsee::proc_macros::rpc;
use ng_infra::rpc_exec;
use ng_infra::server::{RpcHelper, token_identity};
use serde_json::value::RawValue;
use tracing::Instrument;

mod auth;
mod delete;
mod query;

/// `crontab_result` RPC trait 定义，使用 jsonrpsee `#[rpc]` 宏自动生成 Server 端调度代码。
/// 命名空间为 `crontab-result`，分隔符为 `_`（自定义 jsonrpsee fork）。
#[rpc(server, namespace = "crontab-result")]
pub trait Rpc {
    /// 查询定时任务执行结果
    #[method(name = "query")]
    async fn query(&self, token: String, query: CrontabResultDataQuery)
    -> RpcResult<Box<RawValue>>;

    /// 删除定时任务执行结果
    #[method(name = "delete")]
    async fn delete(
        &self,
        token: String,
        query: CrontabResultDataQuery,
    ) -> RpcResult<Box<RawValue>>;
}

/// `crontab_result` RPC 实现结构体，空载体（所有状态通过全局单例获取）。
pub struct CrontabResultRpcImpl;

impl RpcHelper for CrontabResultRpcImpl {}

#[async_trait]
impl RpcServer for CrontabResultRpcImpl {
    async fn query(
        &self,
        token: String,
        query: CrontabResultDataQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "crontab_result", "crontab-result::query", token_key = tk, username = un, query = ?query);
        async { rpc_exec!(query::query(token, query).await) }
            .instrument(span)
            .await
    }

    async fn delete(
        &self,
        token: String,
        query: CrontabResultDataQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "crontab_result", "crontab-result::delete", token_key = tk, username = un, query = ?query);
        async { rpc_exec!(delete::delete(token, query).await) }
            .instrument(span)
            .await
    }
}

/// 构建并返回 `crontab_result` RPC 模块。
pub fn rpc_module() -> jsonrpsee::RpcModule<CrontabResultRpcImpl> {
    CrontabResultRpcImpl.into_rpc()
}
