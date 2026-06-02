//! 统一错误类型定义
//!
//! 定义 `NodegetError` 枚举，覆盖解析、权限、数据库、Agent 连接、
//! 序列化、IO 等场景。所有 crate 通过此类型或 `anyhow::Result` 报告错误。
//! 同时提供错误码映射与 JSON 序列化能力，供 RPC 层统一返回格式。

use thiserror::Error;

/// NodeGet 全局错误枚举，各变体携带描述字符串。
#[derive(Error, Debug, Clone)]
pub enum NodegetError {
    /// 解析失败（格式、语法等）
    #[error("Parse error: {0}")]
    ParseError(String),

    /// 输入校验不通过
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// 权限不足或 Token 校验失败
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// 数据库操作出错
    #[error("Database error: {0}")]
    DatabaseError(String),

    /// 无法连接 Agent
    #[error("Unable to connect agent: {0}")]
    AgentConnectionError(String),

    /// 数据库中未找到指定记录
    #[error("Not found in database: {0}")]
    NotFound(String),

    /// 监控 UUID 不存在
    #[error("UUID not found: {0}")]
    UuidNotFound(String),

    /// 配置项不存在
    #[error("Config not found: {0}")]
    ConfigNotFound(String),

    /// 序列化/反序列化失败
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// IO 操作失败
    #[error("IO error: {0}")]
    IoError(String),

    /// 未归类的其他错误
    #[error("Other error: {0}")]
    Other(String),
}

impl NodegetError {
    /// 返回该错误对应的数值错误码，用于 RPC 响应中的 `error_id` 字段。
    ///
    /// - 解析/序列化/IO 类共享 101
    /// - 权限拒绝 102
    /// - 数据库 103、Agent 连接 104、未找到 105、UUID 未找到 106、配置未找到 107
    /// - 输入校验 108
    /// - 其他 999
    #[must_use]
    pub const fn error_code(&self) -> i128 {
        match self {
            Self::InvalidInput(_) => 108,
            Self::PermissionDenied(_) => 102,
            Self::DatabaseError(_) => 103,
            Self::AgentConnectionError(_) => 104,
            Self::NotFound(_) => 105,
            Self::UuidNotFound(_) => 106,
            Self::ConfigNotFound(_) => 107,
            Self::Other(_) => 999,
            Self::ParseError(_) | Self::SerializationError(_) | Self::IoError(_) => 101,
        }
    }

    /// 将错误转换为 RPC 层使用的 `JsonError` 结构体。
    ///
    /// - 返回包含 `error_id` 与 `error_message` 的 JSON 可序列化对象
    #[must_use]
    pub fn to_json_error(&self) -> crate::utils::JsonError {
        crate::utils::JsonError {
            error_id: self.error_code(),
            error_message: self.to_string(),
        }
    }
}

/// 从 `serde_json::Error` 转换，统一归入序列化错误。
impl From<serde_json::Error> for NodegetError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerializationError(err.to_string())
    }
}

/// 从 `std::io::Error` 转换，统一归入 IO 错误。
impl From<std::io::Error> for NodegetError {
    fn from(err: std::io::Error) -> Self {
        Self::IoError(err.to_string())
    }
}

/// 通用 Result 类型别名，简化函数签名。
pub type Result<T> = anyhow::Result<T>;

/// 尝试将 `anyhow::Error` 还原为 `NodegetError`。
///
/// - 若底层已是 `NodegetError`，则 clone 返回
/// - 否则包装为 `NodegetError::Other`
#[must_use]
pub fn anyhow_to_nodeget_error(err: &anyhow::Error) -> NodegetError {
    if let Some(e) = err.downcast_ref::<NodegetError>() {
        return e.clone();
    }
    NodegetError::Other(err.to_string())
}
