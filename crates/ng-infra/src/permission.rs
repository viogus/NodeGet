//! 权限类型与解析器。
//!
//! [`ScopedPermission<T>`] 是通用范围限制枚举，表达「全部访问」与「限定条目」两种模式。
//!
//! [`PermissionResolver`] 是具体实现（如 Server 端基于 Token 的权限检查器）必须满足的 trait。

use ng_core::permission::data_structure::{Permission, Scope, Token};
use serde::{Deserialize, Serialize};

// ── ScopedPermission ──────────────────────────────────────────────────

/// 权限范围限制枚举。
///
/// - `All` — 无限制，拥有所有范围的完整访问权。
/// - `Scoped(Vec<T>)` — 仅允许访问列表中的条目。
///
/// 使用 `Vec<T>` 而非 `HashSet<T>`，因为只需 `Eq` 约束即可
/// （ng-core 的 `Scope` 类型未实现 `Hash`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopedPermission<T> {
    /// 全部访问——无范围限制
    #[default]
    All,
    /// 受限访问——仅列表中的条目被允许
    Scoped(Vec<T>),
}

impl<T: Eq> ScopedPermission<T> {
    /// 检查指定条目是否被允许访问。
    ///
    /// - `item` — 待检查的条目
    /// - 返回 `true` 表示允许，`false` 表示拒绝
    pub fn is_allowed(&self, item: &T) -> bool {
        match self {
            Self::All => true,
            Self::Scoped(items) => items.contains(item),
        }
    }

    /// 返回是否为无限制模式（`All`）。
    pub const fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// 返回受限列表；若为 `All` 则返回 `None`。
    pub fn as_scoped(&self) -> Option<&[T]> {
        match self {
            Self::All => None,
            Self::Scoped(items) => Some(items),
        }
    }
}

// ── PermissionResolver ────────────────────────────────────────────────

/// Token 权限解析 trait。
///
/// 实现类根据给定的 Token 和 Permission 组合，确定实际生效的范围限制。
pub trait PermissionResolver: Send + Sync {
    /// 解析指定权限的有效范围限制。
    ///
    /// - `token` — 待解析的 Token
    /// - `permission` — 待检查的权限类型
    /// - 返回 [`ScopedPermission::All`] 表示无限制，或 [`ScopedPermission::Scoped`] 列出允许的范围
    fn resolve(&self, token: &Token, permission: &Permission) -> ScopedPermission<Scope>;
}
