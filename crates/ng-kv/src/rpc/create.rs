//! `kv_create` RPC 方法：创建新的 KV 命名空间

use crate::auth::check_kv_create_permission;
use crate::db::create_kv;
use jsonrpsee::core::RpcResult;
use ng_core::error::NodegetError;
use ng_infra::server::to_rpc_error;
use serde_json::value::RawValue;
use tracing::debug;

/// 创建新的 KV 命名空间
///
/// - `token` — 身份令牌，仅 SuperToken 有权限创建
/// - `name` — 命名空间名称，不可重复
///
/// 返回创建的 `KVStore` 序列化结果。
///
/// 内部步骤：
/// 1. 校验 Token 是否为 SuperToken
/// 2. 调用 `create_kv` 写入 marker 记录
/// 3. 序列化结果为 RawValue 返回
pub async fn create(token: String, name: String) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        debug!(target: "kv", namespace = %name, "Processing create namespace request");

        // 检查创建命名空间的权限
        // 保留命名空间由服务端内部维护（如审计数据），RPC 一律不可写
        crate::reserved::ensure_namespace_writable(&name)?;
        check_kv_create_permission(&token).await?;
        debug!(target: "kv", namespace = %name, "Create namespace permission check passed");

        let kv_store = create_kv(name).await?;

        debug!(target: "kv", "Namespace created successfully");

        serde_json::value::to_raw_value(&kv_store)
            .map_err(|e| NodegetError::SerializationError(format!("{e}")).into())
    };

    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => Err(to_rpc_error(&e)),
    }
}
