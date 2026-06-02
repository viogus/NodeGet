//! KV RPC 命名空间：提供 JSON-RPC 方法供客户端操作 KV 存储。
//!
//! 命名空间前缀为 `kv`，包含以下方法：
//! - `kv_create` — 创建命名空间（仅 SuperToken）
//! - `kv_get_value` — 读取单个 key 的值
//! - `kv_get_multi_value` — 批量读取多个 namespace/key 的值
//! - `kv_set_value` — 设置指定 key 的值
//! - `kv_delete_key` — 删除指定 key
//! - `kv_delete_namespace` — 删除整个命名空间
//! - `kv_get_all_keys` — 列出命名空间下所有 key
//! - `kv_list_all_namespace` — 列出所有命名空间

use jsonrpsee::core::RpcResult;
use jsonrpsee::core::async_trait;
use jsonrpsee::proc_macros::rpc;
use ng_infra::rpc_exec;
use ng_infra::server::{RpcHelper, token_identity};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;
use tracing::Instrument;

mod create;
mod delete_key;
mod delete_namespace;
mod get_all_keys;
mod get_multi_value;
mod get_value;
mod list_all_namespace;
mod set_value;

/// 命名空间与 key 的组合项，用于 `get_multi_value` 批量查询
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceKeyItem {
    /// 命名空间名称
    pub namespace: String,
    /// key 名称，支持后缀通配符（如 `metadata_*`）
    pub key: String,
}

/// KV 值条目，表示一个 namespace/key 对应的值
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvValueItem {
    /// 命名空间名称
    pub namespace: String,
    /// key 名称
    pub key: String,
    /// key 对应的 JSON 值，不存在时为 null
    pub value: Value,
}

/// KV RPC 接口定义，所有方法返回 `RpcResult<Box<RawValue>>` 以统一日志格式
#[rpc(server, namespace = "kv")]
pub trait Rpc {
    /// 创建新的 KV 命名空间，仅 SuperToken 有权限
    #[method(name = "create")]
    async fn create(&self, token: String, namespace: String) -> RpcResult<Box<RawValue>>;

    /// 读取指定 namespace 下单个 key 的值
    #[method(name = "get_value")]
    async fn get_value(
        &self,
        token: String,
        namespace: String,
        key: String,
    ) -> RpcResult<Box<RawValue>>;

    /// 批量读取多个 namespace/key 的值，支持后缀通配符
    #[method(name = "get_multi_value")]
    async fn get_multi_value(
        &self,
        token: String,
        namespace_key: Vec<NamespaceKeyItem>,
    ) -> RpcResult<Box<RawValue>>;

    /// 设置指定 namespace 下 key 的值，key 不存在则创建
    #[method(name = "set_value")]
    async fn set_value(
        &self,
        token: String,
        namespace: String,
        key: String,
        value: Value,
    ) -> RpcResult<Box<RawValue>>;

    /// 删除指定 namespace 下的单个 key
    #[method(name = "delete_key")]
    async fn delete_key(
        &self,
        token: String,
        namespace: String,
        key: String,
    ) -> RpcResult<Box<RawValue>>;

    /// 删除整个命名空间及其所有 key，需全局删除权限
    #[method(name = "delete_namespace")]
    async fn delete_namespace(&self, token: String, namespace: String) -> RpcResult<Box<RawValue>>;

    /// 列出指定命名空间下所有 key
    #[method(name = "get_all_keys")]
    async fn get_all_keys(&self, token: String, namespace: String) -> RpcResult<Box<RawValue>>;

    /// 列出所有 KV 命名空间，按 Token 权限过滤可见范围
    #[method(name = "list_all_namespace")]
    async fn list_all_namespace(&self, token: String) -> RpcResult<Box<RawValue>>;
}

/// KV RPC 实现，持有 `RpcHelper` 提供的数据库访问能力
pub struct KvRpcImpl;

impl RpcHelper for KvRpcImpl {}

#[async_trait]
impl RpcServer for KvRpcImpl {
    async fn create(&self, token: String, namespace: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "kv", "kv::create", token_key = tk, username = un, namespace = %namespace);
        async { rpc_exec!(create::create(token, namespace).await) }
            .instrument(span)
            .await
    }

    async fn get_value(
        &self,
        token: String,
        namespace: String,
        key: String,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "kv", "kv::get_value", token_key = tk, username = un, namespace = %namespace, key = %key);
        async { rpc_exec!(get_value::get_value(token, namespace, key).await) }
            .instrument(span)
            .await
    }

    async fn get_multi_value(
        &self,
        token: String,
        namespace_key: Vec<NamespaceKeyItem>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "kv", "kv::get_multi_value", token_key = tk, username = un, items_count = namespace_key.len());
        async { rpc_exec!(get_multi_value::get_multi_value(token, namespace_key).await) }
            .instrument(span)
            .await
    }

    async fn set_value(
        &self,
        token: String,
        namespace: String,
        key: String,
        value: Value,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "kv", "kv::set_value", token_key = tk, username = un, namespace = %namespace, key = %key);
        async { rpc_exec!(set_value::set_value(token, namespace, key, value).await) }
            .instrument(span)
            .await
    }

    async fn delete_key(
        &self,
        token: String,
        namespace: String,
        key: String,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "kv", "kv::delete_key", token_key = tk, username = un, namespace = %namespace, key = %key);
        async { rpc_exec!(delete_key::delete_key(token, namespace, key).await) }
            .instrument(span)
            .await
    }

    async fn delete_namespace(&self, token: String, namespace: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "kv", "kv::delete_namespace", token_key = tk, username = un, namespace = %namespace);
        async { rpc_exec!(delete_namespace::delete_namespace(token, namespace).await) }
            .instrument(span)
            .await
    }

    async fn get_all_keys(&self, token: String, namespace: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "kv", "kv::get_all_keys", token_key = tk, username = un, namespace = %namespace);
        async { rpc_exec!(get_all_keys::get_all_keys(token, namespace).await) }
            .instrument(span)
            .await
    }

    async fn list_all_namespace(&self, token: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "kv", "kv::list_all_namespace", token_key = tk, username = un);
        async { rpc_exec!(list_all_namespace::list_all_namespace(token).await) }
            .instrument(span)
            .await
    }
}

/// 构建 KV RPC 模块，注册所有 `kv_*` 方法，用于合并到服务器 RPC 路由
pub fn rpc_module() -> jsonrpsee::RpcModule<KvRpcImpl> {
    KvRpcImpl.into_rpc()
}
