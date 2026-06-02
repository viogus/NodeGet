//! 定时任务（Crontab）管理 crate：定义定时任务类型与 Server 端调度逻辑。
//!
//! ## 默认 feature（仅类型，Agent 可安全依赖）
//! - [`Cron`] — 定时任务定义
//! - [`CronType`] — Agent 或 Server 端定时任务类型
//! - [`AgentCronType`] — Agent 端定时任务类型（任务下发）
//! - [`ServerCronType`] — Server 端定时任务类型（JS Worker）
//! - [`CrontabResult`] — 定时任务执行结果记录
//! - [`query`] — `CrontabResultQueryCondition`、`CrontabResultDataQuery` 查询 DSL
//!
//! ## `server` feature（仅 Server 二进制启用）
//! - [`cache::CrontabCache`] — 基于 DB 的内存缓存，使用 `DbBackedCache` + `make_global_cache!`
//! - [`server_cron`] — 每分钟调度循环，检测到期任务并触发执行
//! - [`task`] — 定时任务执行：Agent 类型走 Task 下发，Server 类型走 JS Worker
//! - `crontab` RPC 命名空间 — create、delete、edit、get、set_enable
//! - `crontab_result` RPC 命名空间 — query、delete

// ── 通用模块（默认 feature 可见）────────────────────────────────────

mod cron_type;
mod result;

pub use cron_type::{AgentCronType, Cron, CronType, ServerCronType};
pub use result::CrontabResult;
pub mod query;

// ── Server 专属模块 ─────────────────────────────────────────────────

#[cfg(feature = "server")]
pub mod cache;
#[cfg(feature = "server")]
pub mod rpc;
#[cfg(feature = "server")]
pub mod server_cron;
#[cfg(feature = "server")]
pub mod task;

// ── Server 专属 re-export ───────────────────────────────────────────

#[cfg(feature = "server")]
pub use cache::CrontabCache;

#[cfg(feature = "server")]
pub use server_cron::{delete_crontab_by_name, init_crontab_worker, set_crontab_enable_by_name};

#[cfg(feature = "server")]
/// 构建并返回合并了 `crontab` 和 `crontab_result` 两个 RPC 命名空间的模块。
///
/// 调用方应在启动时将其合并到主 RPC 模块：
/// ```ignore
/// main_module.merge(ng_crontab::rpc_module()).unwrap();
/// ```
pub fn rpc_module() -> jsonrpsee::RpcModule<()> {
    let mut module = jsonrpsee::RpcModule::new(());
    module
        .merge(rpc::crontab::rpc_module())
        .expect("Failed to merge crontab RPC");
    module
        .merge(rpc::crontab_result::rpc_module())
        .expect("Failed to merge crontab_result RPC");
    module
}
