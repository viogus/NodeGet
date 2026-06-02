//! Server 端 JSON 序列化与字段变换工具
//!
//! 提供 `RawValue` 序列化、JSON 字段重命名与嵌套字符串解析等能力，
//! 供 RPC 返回值构造和 DB 查询结果后处理使用。

use crate::error::{NodegetError, Result};
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use tracing::error;

/// 将可序列化值转为 `Box<RawValue>`，序列化失败时返回错误。
///
/// - `val`：任何实现 `Serialize` 的值
/// - 返回零拷贝 RawValue 或序列化错误
pub fn to_raw_json<T: Serialize>(val: T) -> Result<Box<RawValue>> {
    serde_json::value::to_raw_value(&val).map_err(|e| {
        error!("Serialization error: {e}");
        NodegetError::SerializationError(e.to_string()).into()
    })
}

/// 将可序列化值转为 `Box<RawValue>`，序列化失败时回退为 `JsonError`。
///
/// - `val`：任何实现 `Serialize` 的值
/// - 返回 RawValue；序列化失败时返回包含错误信息的 RawValue
pub fn to_raw_json_with_fallback<T: Serialize>(val: T) -> Result<Box<RawValue>> {
    serde_json::value::to_raw_value(&val).or_else(|e| {
        error!("Serialization error: {e}");
        let fallback = crate::utils::JsonError {
            error_id: 101,
            error_message: format!("Serialization error: {e}"),
        };
        serde_json::value::to_raw_value(&fallback)
            .map_err(|e| NodegetError::SerializationError(e.to_string()).into())
    })
}

/// 尝试将 Map 中指定 key 的字符串值解析为 JSON 对象。
///
/// DB 查询返回的 JSON 列常被序列化为字符串，此函数将其还原为嵌套结构。
///
/// - `map`：待处理的 JSON Map
/// - `key`：需要解析的字段名
pub fn try_parse_json_field(map: &mut Map<String, Value>, key: &str) {
    if let Some(Value::String(s)) = map.get(key)
        && let Ok(parsed) = serde_json::from_str::<Value>(s)
    {
        map.insert(key.to_string(), parsed);
    }
}

/// 重命名 Map 中的 key，值保持不变。
///
/// - `map`：待处理的 JSON Map
/// - `old_key`：原键名
/// - `new_key`：新键名
pub fn rename_key(map: &mut Map<String, Value>, old_key: &str, new_key: &str) {
    if let Some(v) = map.remove(old_key) {
        map.insert(new_key.to_string(), v);
    }
}

/// 重命名 key 并解析字符串值为 JSON（组合操作）。
///
/// 1. 移除旧 key
/// 2. 若值为字符串则尝试解析为 JSON 结构
/// 3. 以新 key 插入
pub fn rename_and_fix_json(map: &mut Map<String, Value>, old_key: &str, new_key: &str) {
    if let Some(mut value) = map.remove(old_key) {
        if let Value::String(s) = &value
            && let Ok(parsed) = serde_json::from_str::<Value>(s)
        {
            value = parsed;
        }
        map.insert(new_key.to_string(), value);
    }
}
