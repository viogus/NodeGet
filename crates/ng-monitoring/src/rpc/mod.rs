//! RPC 子模块入口。
//!
//! 包含三个 RPC 命名空间的实现：
//! - `agent` — Agent 监控数据的上报、查询、删除
//! - `agent_uuid` — Agent UUID 的管理（列表、软删除）
//! - `nodeget` — Server 级别的辅助 RPC

pub mod agent;
pub mod agent_uuid;
pub mod nodeget;
