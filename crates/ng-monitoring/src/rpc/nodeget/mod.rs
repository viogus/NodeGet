//! `nodeget-server` RPC 命名空间实现（ng-monitoring 部分）。
//!
//! 提供 Server 级别的辅助 RPC 方法，目前仅包含 `list_all_agent_uuid`。
//! 注意：Server 二进制中的 `nodeget-server` 命名空间还包含来自其他 crate 的方法，
//! 此处仅为 ng-monitoring 的贡献部分。

pub mod list_all_agent_uuid;

use jsonrpsee::core::{RpcResult, async_trait};
use jsonrpsee::proc_macros::rpc;
use ng_db::rpc_exec;
use serde_json::value::RawValue;
use tracing::Instrument;

/// `nodeget-server` RPC trait 定义。
#[rpc(server, namespace = "nodeget-server")]
pub trait Rpc {
    /// 列出所有 Agent UUID（根据 Token 权限过滤）
    #[method(name = "list_all_agent_uuid")]
    async fn list_all_agent_uuid(&self, token: String) -> RpcResult<Box<RawValue>>;
}

/// `nodeget-server` RPC 实现。
pub struct NodegetServerRpcImpl;

#[async_trait]
impl RpcServer for NodegetServerRpcImpl {
    async fn list_all_agent_uuid(&self, token: String) -> RpcResult<Box<RawValue>> {
        let span = tracing::info_span!(target: "server", "nodeget-server::list_all_agent_uuid");
        async { rpc_exec!(list_all_agent_uuid::list_all_agent_uuid(token).await) }
            .instrument(span)
            .await
    }
}
