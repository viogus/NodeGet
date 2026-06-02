//! 路由名称规范化 —— 验证和清理 `route_name` 输入。
//!
//! 防止路径遍历攻击：限制字符集为 `[a-zA-Z0-9._-]`，
//! 拒绝纯点组合（`.` / `..`），限制长度 128 字符。

use ng_core::error::NodegetError;
use tracing::warn;

/// 规范化并验证 `route_name`。
///
/// - `route_name` —— 原始路由名称（可选）
///
/// 校验规则：
/// 1. None 直接返回 Ok(None)
/// 2. 去首尾空白后不能为空
/// 3. 长度不超过 128 字符
/// 4. 仅允许 `[a-zA-Z0-9._-]` 字符
/// 5. 不能为纯点组合（`.` / `..`）
pub fn normalize_route_name(route_name: Option<String>) -> anyhow::Result<Option<String>> {
    let Some(raw) = route_name else {
        return Ok(None);
    };

    let normalized = raw.trim().to_owned();
    if normalized.is_empty() {
        warn!(target: "js_worker", "route_name validation failed: empty string");
        return Err(
            NodegetError::InvalidInput("route_name cannot be empty string".to_owned()).into(),
        );
    }

    if normalized.len() > 128 {
        warn!(target: "js_worker", route_name = %normalized, "route_name validation failed: too long");
        return Err(
            NodegetError::InvalidInput("route_name too long (max 128 chars)".to_owned()).into(),
        );
    }

    if !normalized
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        warn!(target: "js_worker", route_name = %normalized, "route_name validation failed: invalid characters");
        return Err(NodegetError::InvalidInput(
            "route_name can only contain [a-zA-Z0-9._-]".to_owned(),
        )
        .into());
    }

    // 显式拒绝 `.` 与 `..` 等纯点组合，避免语义混淆
    if normalized.chars().all(|c| c == '.') {
        warn!(target: "js_worker", route_name = %normalized, "route_name validation failed: all dots");
        return Err(
            NodegetError::InvalidInput("route_name cannot be '.' or '..'".to_owned()).into(),
        );
    }

    Ok(Some(normalized))
}
