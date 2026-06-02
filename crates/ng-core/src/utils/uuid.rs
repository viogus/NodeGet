//! UUID 生成工具

use crate::error::Result;
use uuid::Uuid;

/// 生成随机 UUID（v4）。
///
/// - 返回新生成的 UUID，当前实现不会失败
pub fn generate_random_uuid() -> Result<Uuid> {
    Ok(Uuid::new_v4())
}
