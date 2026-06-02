//! Token 创建请求体定义

use crate::permission::data_structure::Limit;
use serde::{Deserialize, Serialize};

/// Token 创建请求，携带可选凭证与权限限制。
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TokenCreationRequest {
    /// 关联的用户名（可选）
    pub username: Option<String>,
    /// 关联的密码（可选）
    pub password: Option<String>,
    /// Token 有效期起始时间（Unix 毫秒，可选）
    pub timestamp_from: Option<i64>,
    /// Token 有效期截止时间（Unix 毫秒，可选）
    pub timestamp_to: Option<i64>,
    /// Token 版本号，用于区分不同颁发策略
    pub version: Option<i32>,
    /// Token 的权限限制列表
    pub token_limit: Vec<Limit>,
}
