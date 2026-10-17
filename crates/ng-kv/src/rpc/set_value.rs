//! `kv_set_value` RPC 方法：设置指定 namespace 下 key 的值

use crate::auth::check_kv_write_permission;
use crate::db::set_v_to_kv;
use jsonrpsee::core::RpcResult;
use ng_core::error::NodegetError;
use ng_infra::server::to_rpc_error;
use serde_json::Value;
use serde_json::value::RawValue;
use tracing::debug;

/// 设置指定 namespace 下 key 的值，key 不存在则自动创建
///
/// - `token` — 身份令牌，需拥有该 namespace/key 的 `Kv::Write` 权限
/// - `namespace` — 命名空间名称
/// - `key` — 要设置的 key
/// - `value` — 要设置的 JSON 值
///
/// 返回 `{"success":true}`。
///
/// 内部步骤：
/// 1. 校验 Token 是否拥有该 key 的写权限
/// 2. 调用 `set_v_to_kv` 执行 upsert（存在则更新，不存在则插入）
/// 3. 返回成功标识
pub async fn set_value(
    token: String,
    namespace: String,
    key: String,
    value: Value,
) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        debug!(target: "kv", namespace = %namespace, key = %key, "Processing set_value request");

        // 检查写权限
        // 保留命名空间由服务端内部维护（如审计数据），RPC 一律不可写
        crate::reserved::ensure_namespace_writable(&namespace)?;
        check_kv_write_permission(&token, &namespace, &key).await?;
        debug!(target: "kv", namespace = %namespace, key = %key, "set_value permission check passed");

        set_v_to_kv(namespace, key, value).await?;

        debug!(target: "kv", "set_value completed");

        serde_json::value::to_raw_value(&serde_json::json!({"success": true}))
            .map_err(|e| NodegetError::SerializationError(format!("{e}")).into())
    };

    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => Err(to_rpc_error(&e)),
    }
}
