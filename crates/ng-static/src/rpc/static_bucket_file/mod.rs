//! `static-bucket-file` RPC 命名空间定义与实现。
//!
//! 职责：定义文件级操作（upload / read / delete / rename / list）的 RPC trait，
//! 并在 `StaticBucketFileRpcImpl` 中实现各方法，统一走 `rpc_exec!` 宏完成
//! 鉴权 -> 业务逻辑 -> 序列化的流程。
//!
//! 协作关系：各方法委托到对应的子模块，由服务器二进制在 `build_modules()` 中
//! 合并到主 RpcModule。

use jsonrpsee::core::RpcResult;
use jsonrpsee::core::async_trait;
use jsonrpsee::proc_macros::rpc;
use ng_infra::rpc_exec;
use ng_infra::server::{RpcHelper, token_identity};
use serde_json::value::RawValue;
use tracing::Instrument;

mod auth;
mod delete_file;
mod list_file;
mod read_file;
mod rename_file;
mod upload_file;

/// `static-bucket-file` RPC trait，命名空间 `static-bucket-file`。
///
/// 所有方法第一个参数为 `token`，返回 `RpcResult<Box<RawValue>>`，
/// 与 `rpc_exec!` 宏配合实现统一日志与错误映射。
#[rpc(server, namespace = "static-bucket-file")]
pub trait Rpc {
    /// 上传文件到指定桶（需 Write 权限，body 与 base64 二选一）。
    #[method(name = "upload")]
    async fn upload(
        &self,
        token: String,
        name: String,
        path: String,
        body: Option<Vec<u8>>,
        base64: Option<String>,
    ) -> RpcResult<Box<RawValue>>;

    /// 读取桶内文件内容，返回 Base64 编码（需 Read 权限）。
    #[method(name = "read")]
    async fn read(&self, token: String, name: String, path: String) -> RpcResult<Box<RawValue>>;

    /// 删除桶内文件（需 Delete 权限）。
    #[method(name = "delete")]
    async fn delete(&self, token: String, name: String, path: String) -> RpcResult<Box<RawValue>>;

    /// 重命名桶内文件（需 Write + Delete 权限）。
    #[method(name = "rename")]
    async fn rename(
        &self,
        token: String,
        name: String,
        from: String,
        to: String,
    ) -> RpcResult<Box<RawValue>>;

    /// 列出桶内所有文件的元数据（需 List 权限）。
    #[method(name = "list")]
    async fn list(&self, token: String, name: String) -> RpcResult<Box<RawValue>>;
}

/// `static-bucket-file` RPC 的具体实现，空结构体 + `RpcHelper` 默认方法。
pub struct StaticBucketFileRpcImpl;

impl RpcHelper for StaticBucketFileRpcImpl {}

#[async_trait]
impl RpcServer for StaticBucketFileRpcImpl {
    async fn upload(
        &self,
        token: String,
        name: String,
        path: String,
        body: Option<Vec<u8>>,
        base64: Option<String>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket_file", "static-bucket-file::upload", token_key = tk, username = un, name = %name, path = %path, has_body = body.is_some(), has_base64 = base64.is_some());
        async { rpc_exec!(upload_file::upload_file_rpc(token, name, path, body, base64).await) }
            .instrument(span)
            .await
    }

    async fn read(&self, token: String, name: String, path: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket_file", "static-bucket-file::read", token_key = tk, username = un, name = %name, path = %path);
        async { rpc_exec!(read_file::read_file_rpc(token, name, path).await) }
            .instrument(span)
            .await
    }

    async fn delete(&self, token: String, name: String, path: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket_file", "static-bucket-file::delete", token_key = tk, username = un, name = %name, path = %path);
        async { rpc_exec!(delete_file::delete_file_rpc(token, name, path).await) }
            .instrument(span)
            .await
    }

    async fn rename(
        &self,
        token: String,
        name: String,
        from: String,
        to: String,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket_file", "static-bucket-file::rename", token_key = tk, username = un, name = %name, from = %from, to = %to);
        async { rpc_exec!(rename_file::rename_file_rpc(token, name, from, to).await) }
            .instrument(span)
            .await
    }

    async fn list(&self, token: String, name: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "static_bucket_file", "static-bucket-file::list", token_key = tk, username = un, name = %name);
        async { rpc_exec!(list_file::list_file_rpc(token, name).await) }
            .instrument(span)
            .await
    }
}
