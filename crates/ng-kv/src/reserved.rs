//! 保留命名空间：由服务端内部维护、KV RPC 不允许写入的命名空间。
//!
//! 背景：JS Worker 的 `env.token` 是 ID 为 1 的超级令牌（见 `ng_js_runtime::js_audit`
//! 的模块文档），因此 `check_kv_*` 权限校验对它一律放行。审计这类「为了监管而存在」
//! 的数据如果落在普通可写命名空间里，被审计方自己就能把它关掉或抹掉。
//!
//! 约定：命名空间以 [`RESERVED_NAMESPACE_PREFIX`] 开头即为服务端保留。
//! 写入类 RPC（`kv_create` / `kv_set_value` / `kv_delete_namespace` / `kv_delete_key`）
//! 必须先调用 [`ensure_namespace_writable`]；读取类不拦，方便运维通过 KV 编辑器查看。

use ng_core::error::NodegetError;

/// 保留命名空间前缀。
pub const RESERVED_NAMESPACE_PREFIX: &str = "__";

/// 判断命名空间是否为服务端保留。
#[must_use]
pub fn is_reserved_namespace(namespace: &str) -> bool {
    namespace.starts_with(RESERVED_NAMESPACE_PREFIX)
}

/// 写入类 RPC 的守卫：保留命名空间拒绝写入。
///
/// # Errors
///
/// 命名空间为服务端保留时返回 `NodegetError::PermissionDenied`。
pub fn ensure_namespace_writable(namespace: &str) -> Result<(), NodegetError> {
    if is_reserved_namespace(namespace) {
        return Err(NodegetError::PermissionDenied(format!(
            "namespace `{namespace}` is reserved for server-internal use and cannot be written via RPC"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_prefix_is_double_underscore() {
        assert!(is_reserved_namespace("__js_audit"));
        assert!(is_reserved_namespace("__"));
        assert!(!is_reserved_namespace("js_audit"));
        assert!(!is_reserved_namespace("_js_audit"));
        assert!(!is_reserved_namespace(""));
    }

    #[test]
    fn writes_to_reserved_namespaces_are_rejected() {
        assert!(ensure_namespace_writable("__js_policy").is_err());
        assert!(ensure_namespace_writable("global").is_ok());
        assert!(ensure_namespace_writable("js_audit").is_ok());
    }
}
