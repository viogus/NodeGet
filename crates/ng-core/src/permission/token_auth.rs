//! Token / Auth 双模式认证类型
//!
//! RPC 调用支持两种认证方式：key:secret Token 或 username|password 账号认证。
//! `TokenOrAuth` 将两者统一抽象，供鉴权层无差别处理。

use serde::{Deserialize, Serialize};

/// 认证凭证：Token 模式或账号密码模式
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenOrAuth {
    /// key:secret 格式的 Token 认证
    Token(String, String),
    /// username|password 格式的账号认证
    Auth(String, String),
}

impl TokenOrAuth {
    /// 从完整凭证字符串解析认证模式。
    ///
    /// - `full_token`：`key:secret` 或 `username|password` 格式的字符串
    /// - 返回解析后的 `TokenOrAuth`，格式非法时返回 Err
    pub fn from_full_token(full_token: &str) -> Result<Self, String> {
        if let Some((key, secret)) = full_token.split_once(':') {
            Ok(Self::Token(key.to_string(), secret.to_string()))
        } else if let Some((username, password)) = full_token.split_once('|') {
            Ok(Self::Auth(username.to_string(), password.to_string()))
        } else {
            Err("Invalid token format: must be 'key:secret' or 'username|password'".to_string())
        }
    }

    /// 返回 Token 的 key 部分，账号模式返回 None。
    #[must_use]
    pub fn token_key(&self) -> Option<&str> {
        match self {
            Self::Token(key, _) => Some(key),
            Self::Auth(_, _) => None,
        }
    }

    /// 返回 Token 的 secret 部分，账号模式返回 None。
    #[must_use]
    pub fn token_secret(&self) -> Option<&str> {
        match self {
            Self::Token(_, secret) => Some(secret),
            Self::Auth(_, _) => None,
        }
    }

    /// 返回账号模式的用户名，Token 模式返回 None。
    #[must_use]
    pub fn username(&self) -> Option<&str> {
        match self {
            Self::Token(_, _) => None,
            Self::Auth(username, _) => Some(username),
        }
    }

    /// 返回账号模式的密码，Token 模式返回 None。
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        match self {
            Self::Token(_, _) => None,
            Self::Auth(_, password) => Some(password),
        }
    }

    /// 判断是否为 Token 模式。
    #[must_use]
    pub const fn is_token(&self) -> bool {
        matches!(self, Self::Token(_, _))
    }

    /// 判断是否为账号密码模式。
    #[must_use]
    pub const fn is_auth(&self) -> bool {
        matches!(self, Self::Auth(_, _))
    }
}
