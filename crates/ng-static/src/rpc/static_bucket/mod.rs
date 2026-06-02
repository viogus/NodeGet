//! `static-bucket` RPC 命名空间定义与实现。
//!
//! 职责：定义桶级 CRUD 的 RPC trait（create / read / update / delete / list），
//! 并在 `StaticBucketRpcImpl` 中实现各方法，统一走 `rpc_exec!` 宏完成
//! 鉴权 -> 业务逻辑 -> 序列化的流程。
//!
//! 协作关系：各方法委托到 `auth`、`create`、`read`、`update`、`delete`、`list`
//! 子模块，由服务器二进制在 `build_modules()` 中合并到主 RpcModule。

use jsonrpsee::core::RpcResult;
use jsonrpsee::core::async_trait;
use jsonrpsee::proc_macros::rpc;
use ng_infra::rpc_exec;
use ng_infra::server::{RpcHelper, token_identity};
use serde_json::value::RawValue;
use tracing::Instrument;

mod auth;
mod create;
mod delete;
mod list;
mod read;
mod update;

/// `static-bucket` RPC trait，命名空间 `static-bucket`。
///
/// 所有方法第一个参数为 `token`，返回 `RpcResult<Box<RawValue>>`，
/// 与 `rpc_exec!` 宏配合实现统一日志与错误映射。
#[rpc(server, namespace = "static-bucket")]
pub trait Rpc {
    /// 创建新的静态文件桶（需 Write 权限）。
    #[method(name = "create")]
    async fn create(
        &self,
        token: String,
        name: String,
        path: String,
        is_http_root: bool,
        cors: bool,
    ) -> RpcResult<Box<RawValue>>;

    /// 读取指定桶的配置信息（需 Read 权限）。
    #[method(name = "read")]
    async fn read(&self, token: String, name: String) -> RpcResult<Box<RawValue>>;

    /// 更新桶配置（需 Write 权限）。
    #[method(name = "update")]
    async fn update(
        &self,
        token: String,
        name: String,
        path: String,
        is_http_root: bool,
        cors: bool,
        enable: Option<bool>,
    ) -> RpcResult<Box<RawValue>>;

    /// 删除桶（需 Delete 权限）。
    #[method(name = "delete")]
    async fn delete(&self, token: String, name: String) -> RpcResult<Box<RawValue>>;

    /// 列出所有桶名称（仅 SuperToken）。
    #[method(name = "list")]
    async fn list(&self, token: String) -> RpcResult<Box<RawValue>>;
}

/// `static-bucket` RPC 的具体实现，空结构体 + `RpcHelper` 默认方法。
pub struct StaticBucketRpcImpl;

impl RpcHelper for StaticBucketRpcImpl {}

#[async_trait]
impl RpcServer for StaticBucketRpcImpl {
    async fn create(
        &self,
        token: String,
        name: String,
        path: String,
        is_http_root: bool,
        cors: bool,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket", "static-bucket::create", token_key = tk, username = un, name = %name, path = %path, is_http_root = is_http_root, cors = cors);
        async { rpc_exec!(create::create(token, name, path, is_http_root, cors).await) }
            .instrument(span)
            .await
    }

    async fn read(&self, token: String, name: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket", "static-bucket::read", token_key = tk, username = un, name = %name);
        async { rpc_exec!(read::read(token, name).await) }
            .instrument(span)
            .await
    }

    async fn update(
        &self,
        token: String,
        name: String,
        path: String,
        is_http_root: bool,
        cors: bool,
        enable: Option<bool>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket", "static-bucket::update", token_key = tk, username = un, name = %name, path = %path, is_http_root = is_http_root, cors = cors, enable = ?enable);
        async { rpc_exec!(update::update(token, name, path, is_http_root, cors, enable).await) }
            .instrument(span)
            .await
    }

    async fn delete(&self, token: String, name: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket", "static-bucket::delete", token_key = tk, username = un, name = %name);
        async { rpc_exec!(delete::delete(token, name).await) }
            .instrument(span)
            .await
    }

    async fn list(&self, token: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket", "static-bucket::list", token_key = tk, username = un);
        async { rpc_exec!(list::list_rpc(token).await) }
            .instrument(span)
            .await
    }
}
