//! JS Worker RPC 调用审计（**观察模式，不参与裁决**）。
//!
//! ## 背景
//!
//! JS Worker 的 `env.token` 由部署侧（board 的 bootstrap 与 base-worker 的
//! `upsertWorker`）写入，当前等于 ID 为 1 的超级令牌，而 `check_token_limit`
//! 对超级令牌无条件放行 —— 也就是说服务端对 JS 实际上没有任何约束。board 属上游
//! 维护、不便于改动，因此先在服务端唯一的 JS→RPC 漏斗上做审计：
//!
//! ```text
//! JS: nodeget() → __nodeget_rpc_raw → js_nodeget() → 进程内 RpcModule
//! ```
//!
//! 目标是为后续「按 worker 白名单强制」（忽略 token 的超级身份）提供**实测**依据，
//! 而不是靠人读脚本猜权限。
//!
//! ## 行为约定
//!
//! - **只观察不拦截**：任何调用都原样放行，不影响现有行为，可随时回退；
//! - 内存聚合每个 `(worker, method)` 的调用次数与首末时间（进程内，不落库）；
//! - 首次见到某个组合时打一条 `WARN`（生产 `log_filter = "warn"` 下可见），
//!   敏感方法（执行 SQL / token / terminal / db / delete 类）**每次都打**；
//! - 每 [`FLUSH_INTERVAL`] 把累计快照写入 KV 命名空间 [`AUDIT_NAMESPACE`]
//!   （key = worker 名），便于 `psql` 或 KV 编辑器直接查看；
//! - 运行时开关：KV `js_policy/__audit` 设为 `{"enabled": false}` 即关闭审计，
//!   无需重新发版；读取失败时保持当前值（默认开启）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use tracing::{debug, warn};

use crate::spawn_on_server_runtime::spawn_detached_on_server_runtime;

/// 审计数据落盘的 KV 命名空间
pub const AUDIT_NAMESPACE: &str = "js_audit";
/// 运行时开关所在的 KV 命名空间
pub const POLICY_NAMESPACE: &str = "js_policy";
/// 运行时开关的 key（value 形如 `{"enabled": false}`）
pub const AUDIT_SWITCH_KEY: &str = "__audit";
/// 落盘间隔
const FLUSH_INTERVAL: Duration = Duration::from_mins(5);
/// 未取到 worker 名时使用的占位（例如绕过包装器直调 `__nodeget_rpc_raw`）
const UNKNOWN_WORKER: &str = "<unknown>";

/// 每次都告警的敏感方法（前缀匹配）：执行 SQL、凭据管理、终端、任意库操作、
/// 删除类操作。观察期一旦出现这些调用就应当人工确认。
const SENSITIVE_METHODS: &[&str] = &[
    "nodeget-server_exec_sql",
    "nodeget-server_get_database_type",
    "token_",
    "terminal",
    "db_",
    "js-worker_delete",
    "js-worker_update",
    "crontab_delete",
    "kv_delete",
];

/// 审计开关，默认开启；由 [`refresh_switch`] 周期性从 KV 刷新。
static ENABLED: AtomicBool = AtomicBool::new(true);
/// flush 循环是否已启动（幂等）
static FLUSH_STARTED: AtomicBool = AtomicBool::new(false);

/// 单个 `(worker, method)` 组合的累计统计
#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    count: u64,
    first_seen_ms: i64,
    last_seen_ms: i64,
}

type AuditTable = HashMap<(String, String), Entry>;

fn table() -> &'static Mutex<AuditTable> {
    static TABLE: OnceLock<Mutex<AuditTable>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> i64 {
    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

/// 是否为敏感方法（前缀匹配，大小写敏感）
#[must_use]
pub fn is_sensitive(method: &str) -> bool {
    SENSITIVE_METHODS
        .iter()
        .any(|prefix| method.starts_with(prefix))
}

/// 记录一次 JS 发起的 RPC 调用。**不做任何拦截**，只聚合与告警。
///
/// - `worker` —— 发起调用的 worker 名；`None` 表示未能取得（记为 `<unknown>`）
/// - `method` —— JSON-RPC 方法名（形如 `task_create_task_blocking`）
pub fn record(worker: Option<&str>, method: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let worker = worker.unwrap_or(UNKNOWN_WORKER);
    let now = now_ms();

    let is_first_seen = {
        let mut counters = table().lock().unwrap_or_else(PoisonError::into_inner);
        let entry = counters
            .entry((worker.to_owned(), method.to_owned()))
            .or_insert_with(|| Entry {
                count: 0,
                first_seen_ms: now,
                last_seen_ms: now,
            });
        entry.count += 1;
        entry.last_seen_ms = now;
        let first = entry.count == 1;
        // 显式释放锁：下面的告警日志可能较慢，不应持锁
        drop(counters);
        first
    };

    let sensitive = is_sensitive(method);
    if is_first_seen || sensitive {
        warn!(
            target: "js_audit",
            worker,
            method,
            first_seen = is_first_seen,
            sensitive,
            "js worker rpc call"
        );
    }
}

/// 确保 flush 循环已在服务器 Runtime 上启动（幂等）。每次 JS RPC 调用都会顺带调用。
pub fn ensure_flusher() {
    if FLUSH_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(e) = spawn_detached_on_server_runtime(flush_loop()) {
        // Handle 未初始化（例如测试环境）：退化为「只在日志里告警、不落盘」
        warn!(target: "js_audit", error = %e, "js audit flusher not started");
    }
}

async fn flush_loop() {
    loop {
        tokio::time::sleep(FLUSH_INTERVAL).await;
        refresh_switch().await;
        if let Err(e) = flush_once().await {
            warn!(target: "js_audit", error = %e, "js audit flush failed");
        }
    }
}

/// 从 KV 刷新运行时开关；读取失败保持当前值（fail-open 到「保持现状」）
async fn refresh_switch() {
    match ng_kv::get_v_from_kv_lenient(POLICY_NAMESPACE, AUDIT_SWITCH_KEY).await {
        Ok(Some(value)) => {
            let enabled = value
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let previous = ENABLED.swap(enabled, Ordering::Relaxed);
            if previous != enabled {
                warn!(target: "js_audit", enabled, "js audit switch changed");
            }
        }
        Ok(None) => {
            ENABLED.store(true, Ordering::Relaxed);
        }
        Err(e) => {
            debug!(target: "js_audit", error = %e, "failed to read js audit switch, keeping current value");
        }
    }
}

/// 把累计快照写入 KV。内存中的计数**不清零**，KV 里始终是累计值。
async fn flush_once() -> anyhow::Result<()> {
    let snapshot = snapshot();
    if snapshot.is_empty() {
        return Ok(());
    }
    // 命名空间不存在时 set_v_to_kv 会报错，这里先确保其存在（幂等）
    ng_kv::get_or_create_kv(AUDIT_NAMESPACE.to_owned()).await?;
    let workers = snapshot.len();
    for (worker, payload) in snapshot {
        ng_kv::set_v_to_kv(AUDIT_NAMESPACE.to_owned(), worker, payload).await?;
    }
    debug!(target: "js_audit", workers, "js audit flushed");
    Ok(())
}

/// 生成按 worker 分组的累计快照：`worker -> {updated_at_ms, methods: {...}}`
fn snapshot() -> Vec<(String, Value)> {
    let counters = table().lock().unwrap_or_else(PoisonError::into_inner);
    if counters.is_empty() {
        return Vec::new();
    }

    let mut grouped: HashMap<String, Map<String, Value>> = HashMap::new();
    for ((worker, method), entry) in counters.iter() {
        grouped.entry(worker.clone()).or_default().insert(
            method.clone(),
            json!({
                "count": entry.count,
                "first_seen_ms": entry.first_seen_ms,
                "last_seen_ms": entry.last_seen_ms,
            }),
        );
    }
    // 快照已完全拥有数据，尽早释放锁
    drop(counters);

    let updated_at_ms = now_ms();
    grouped
        .into_iter()
        .map(|(worker, methods)| {
            let payload = json!({
                "worker": worker,
                "updated_at_ms": updated_at_ms,
                "methods": methods,
            });
            (worker, payload)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{SENSITIVE_METHODS, is_sensitive, snapshot};
    use std::sync::atomic::Ordering;

    #[test]
    fn sensitive_methods_match_by_prefix() {
        assert!(is_sensitive("nodeget-server_exec_sql"));
        assert!(is_sensitive("token_edit"));
        assert!(is_sensitive("kv_delete_namespace"));
        assert!(is_sensitive("js-worker_update"));
        assert!(!is_sensitive("kv_get_value"));
        assert!(!is_sensitive("task_create_task_blocking"));
        // 空字符串不应命中任何前缀（防止误报）
        assert!(!is_sensitive(""));
    }

    #[test]
    fn sensitive_list_has_no_empty_prefix() {
        assert!(
            SENSITIVE_METHODS.iter().all(|p| !p.is_empty()),
            "空前缀会让所有方法都被视为敏感"
        );
    }

    #[test]
    fn record_aggregates_and_respects_runtime_switch() {
        // 本模块共享全局状态（聚合表 + 开关），因此把断言合成单个测试，避免并行测试
        // 在「关闭开关」的窗口内互相干扰；worker 名保持唯一，避免与其它测试串味。
        let worker = "test-worker-agg";
        super::record(Some(worker), "task_create_task_blocking");
        super::record(Some(worker), "task_create_task_blocking");
        super::record(None, "kv_get_value");

        let snap = snapshot();
        let entry = snap
            .iter()
            .find(|(name, _)| name == worker)
            .expect("worker present in snapshot");
        let count = entry
            .1
            .get("methods")
            .and_then(|methods| methods.get("task_create_task_blocking"))
            .and_then(|m| m.get("count"))
            .and_then(serde_json::Value::as_u64)
            .expect("count present");
        assert_eq!(count, 2, "同一方法应累加计数");
        assert!(
            snap.iter().any(|(name, _)| name == super::UNKNOWN_WORKER),
            "unknown worker should be recorded under placeholder"
        );

        // 关闭开关后不应再记录任何调用
        let disabled_worker = "test-worker-disabled";
        super::ENABLED.store(false, Ordering::Relaxed);
        super::record(Some(disabled_worker), "kv_get_value");
        super::ENABLED.store(true, Ordering::Relaxed);

        let snap_after = snapshot();
        assert!(
            !snap_after.iter().any(|(name, _)| name == disabled_worker),
            "关闭审计后不应记录任何调用"
        );
    }
}
