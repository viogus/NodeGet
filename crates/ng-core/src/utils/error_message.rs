//! RPC 错误消息构造工具
//!
//! 将各类错误转换为 RPC 层所需的 `Box<RawValue>` 或 `serde_json::Value` 格式，
//! 供 `rpc_exec!` 宏统一返回。

use crate::error::{NodegetError, anyhow_to_nodeget_error};
use anyhow::Result;
use serde_json::value::RawValue;

/// 构造 `serde_json::Value` 格式的错误响应。
///
/// - `error_id`：数值错误码
/// - `error_message`：错误描述
/// - 返回 JSON Value，序列化失败时回退为硬编码错误
pub fn generate_error_message(error_id: impl Into<i128>, error_message: &str) -> serde_json::Value {
    let json_error = crate::utils::JsonError {
        error_id: error_id.into(),
        error_message: error_message.to_string(),
    };
    serde_json::to_value(json_error).unwrap_or_else(|_| {
        serde_json::json!({
            "error_id": 101,
            "error_message": "Failed to serialize error"
        })
    })
}

/// 将错误码和消息转换为 `Box<RawValue>`，用于 RPC 直接返回。
///
/// - `code`：数值错误码
/// - `msg`：错误描述
/// - 返回 `Box<RawValue>` 或序列化失败的错误
pub fn error_to_raw(code: impl Into<i128>, msg: &str) -> Result<Box<RawValue>> {
    let json_error = crate::utils::JsonError {
        error_id: code.into(),
        error_message: msg.to_string(),
    };
    serde_json::value::to_raw_value(&json_error)
        .map_err(|e| NodegetError::SerializationError(e.to_string()).into())
}

/// 将 `NodegetError` 转换为 `Box<RawValue>`。
///
/// - `error`：已分类的 NodeGet 错误
/// - 返回包含 `error_id` 与 `error_message` 的 RawValue
pub fn nodeget_error_to_raw(error: &NodegetError) -> Result<Box<RawValue>> {
    let json_error = error.to_json_error();
    serde_json::value::to_raw_value(&json_error)
        .map_err(|e| NodegetError::SerializationError(e.to_string()).into())
}

/// 将 `anyhow::Error` 转换为 `Box<RawValue>`。
///
/// 1. 尝试将 anyhow 错误 downcast 为 `NodegetError`
/// 2. 转换为 `JsonError` 再序列化为 RawValue
pub fn anyhow_error_to_raw(error: &anyhow::Error) -> Result<Box<RawValue>> {
    let nodeget_error = anyhow_to_nodeget_error(error);
    nodeget_error_to_raw(&nodeget_error)
}
