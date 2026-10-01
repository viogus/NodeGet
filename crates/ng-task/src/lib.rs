//! ng-task: Task types and RPC namespace for NodeGet.
//!
//! ## Default features (types only)
//! - `TaskEventType`, `TaskEvent`, `TaskEventResult` — task type definitions
//! - `TaskEventResponse` — task result upload structure
//! - `WebShellTask`, `ExecuteTask`, `HttpRequestTask`, `DnsTask` — parameter types
//! - `DnsRecordResult`, `HttpRequestTaskResult` — result types
//! - `query` module — `TaskQueryCondition`, `TaskDataQuery`
//!
//! ## `server` feature
//! - `TaskManager` — broadcast channel manager for task events
//! - Task RPC namespace — `task.*` JSON-RPC methods
//! - `MonitoringUuidProvider` — trait for UUID cache operations (injected by server)
//! - `retention` module — 服务端数据库保留期清理（`task` / `crontab_result` / `js_result`）

pub mod types;

// ── 共享工具 ────────────────────────────────────────────────────────

/// 当前 Unix 时间戳（毫秒）。
///
/// 刻意不引入 `chrono`：ng-task 的默认 feature 依赖集保持最小（Agent 侧复用本 crate），
/// 标准库足以覆盖服务端写 `created_at` 与保留期清理的取时需求。
#[cfg(feature = "server")]
pub(crate) fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

// Re-export types at crate root for convenience
pub use types::query;
pub use types::{
    DnsRecordResult, DnsRecordType, DnsTask, ExecuteTask, HttpRequestTask, HttpRequestTaskResult,
    TaskEvent, TaskEventResponse, TaskEventResult, TaskEventType, WebShellTask,
};

// ── Server-only modules ─────────────────────────────────────────────

#[cfg(feature = "server")]
pub mod rpc;

#[cfg(feature = "server")]
pub mod retention;

#[cfg(feature = "server")]
pub use rpc::{
    MonitoringUuidProvider, TaskManager, monitoring_uuid_provider, rpc_module,
    set_monitoring_uuid_provider,
};
