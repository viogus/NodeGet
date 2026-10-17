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
//! - 运行时开关：KV `__js_policy/__audit` 设为 `{"enabled": false}` 即关闭审计，
//!   无需重新发版；读取失败时保持当前值（默认开启）。
//!   两个命名空间都以 `__` 开头，`kv_*` RPC 不可写，因此被审计的 worker
//!   （`env.token` 是超级令牌）无法用常规 KV 接口关闭审计或抹掉落盘数据。
//!
//!   **能力边界（必须知道）**：worker 仍可通过 `nodeget-server_exec_sql` 直接对
//!   `kv` 表执行任意 SQL（该 RPC 本身就是完全信任的，见 CLAUDE.md）。也就是说
//!   保留命名空间挡住的是「顺手一条 kv_set_value」这条廉价路径，不是有决心的对手。
//!   能对抗它的只有两件事：其一，`execSql` 属于敏感方法，调用本身会先留下 WARN；
//!   其二，根治办法是不把超级令牌交给 JS（属于 board/上游的改动）。
//!
//! ## 归属与容量
//!
//! - worker 名以 Rust 侧线程本地为准（`prepare_invoke_globals` 写入），JS 自报的值
//!   只作对照，不一致时记 `worker_mismatch`；
//! - 聚合表有容量上限，超出的新键折叠进 `<overflow>`，键组成部分截断到 128 字符；
//! - 敏感方法按「前 3 次 + 每 60 次」限流告警，避免路由处理器每请求刷一行 WARN。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use tracing::{debug, warn};

use crate::spawn_on_server_runtime::spawn_detached_on_server_runtime;

/// 审计数据落盘的 KV 命名空间。
///
/// 以 `__` 开头是**服务端保留**前缀：`kv_create` / `kv_set_value` / `kv_delete_*`
/// 一律拒绝写入（见 `ng_kv::reserved`）。被审计的 worker 持超级令牌，
/// 若不保留，它自己就能把审计开关关掉、把证据删掉。
pub const AUDIT_NAMESPACE: &str = "__js_audit";
/// 运行时开关所在的 KV 命名空间（同样保留，worker 改不了）
pub const POLICY_NAMESPACE: &str = "__js_policy";
/// 运行时开关的 key（value 形如 `{"enabled": false}`）
pub const AUDIT_SWITCH_KEY: &str = "__audit";
/// 落盘间隔
const FLUSH_INTERVAL: Duration = Duration::from_mins(5);
/// 未取到 worker 名时使用的占位（例如绕过包装器直调 `__nodeget_rpc_raw`）
const UNKNOWN_WORKER: &str = "<unknown>";

/// 聚合表容量上限。键的组成部分（worker / method）都来自脚本一侧，
/// 不设上限时脚本可以无限制造新键把内存撑爆。
const MAX_ENTRIES: usize = 4_096;

/// 超出容量后所有新键折叠到这个占位键上，保证内存有界且不丢「有异常」这个信号。
const OVERFLOW_WORKER: &str = "<overflow>";

/// 单个键组成部分的最大长度（字符）。日志与 KV key 都要用它，避免一行变几兆。
const MAX_KEY_CHARS: usize = 128;

/// 每次都告警的敏感方法（前缀匹配）：执行 SQL、凭据管理、终端、任意库操作、
/// 删除类操作。观察期一旦出现这些调用就应当人工确认。
const SENSITIVE_METHODS: &[&str] = &[
    // 执行 SQL / 数据库
    "nodeget-server_exec_sql",
    "nodeget-server_get_database_type",
    "db_",
    // 配置与自更新
    "nodeget-server_edit_config",
    "nodeget-server_self_update",
    "nodeget-server_read_config",
    // 凭据
    "token_",
    // 终端
    "terminal",
    // 删除类（含 KV / Agent / 静态资源 / 结果）
    "kv_delete",
    "kv_set_value",
    "task_delete",
    "js-worker_delete",
    "js-worker_update",
    "js-worker_create",
    "js-worker_run",
    "js-result_delete",
    "crontab_delete",
    "crontab-result_delete",
    "static-bucket_delete",
    "static-bucket-file_delete",
    "static-bucket-file_upload",
    "agent_delete",
    "agent-uuid_delete",
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
    /// 已就此组合打过多少次 WARN；用于做频率限制（敏感方法原来每次都打）
    warned: u32,
}

type AuditTable = HashMap<(String, String), Entry>;

/// 仅测试用：清空聚合表（表是进程级全局状态）。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    table()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
}

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

thread_local! {
    /// 当前 OS 线程上正在执行的 worker 名，由 [`set_current_worker`] 在每次执行前写入。
    ///
    /// 运行时池是「一个 worker 一个 OS 线程、一个 QuickJS context」，所以线程本地
    /// 就是 Rust 侧的**权威**归属。JS 侧传上来的名字只是 hint：被审计的脚本可以
    /// 自己改 `globalThis.__nodeget_current_script_name`，不能作为取证依据。
    static CURRENT_WORKER: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// 记录当前线程正在执行的 worker（由 `prepare_invoke_globals` 在每次执行前调用）。
pub fn set_current_worker(name: Option<&str>) {
    CURRENT_WORKER.with(|cell| {
        *cell.borrow_mut() = name.map(str::to_owned);
    });
}

/// 清除当前线程的 worker 记录（执行结束时调用，避免下一次执行串味）。
pub fn clear_current_worker() {
    CURRENT_WORKER.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// 截断过长的键组成部分，保证日志行与 KV key 有界。
fn bounded(value: &str) -> String {
    if value.chars().count() <= MAX_KEY_CHARS {
        return value.to_owned();
    }
    let truncated: String = value.chars().take(MAX_KEY_CHARS).collect();
    format!("{truncated}…({} chars)", value.chars().count())
}

/// 记录一次 JS 发起的 RPC 调用。**不做任何拦截**，只聚合与告警。
///
/// worker 归属以 Rust 侧线程本地为准（见 [`set_current_worker`]）；`worker_hint`
/// 是 JS 传上来的值，只作为对照：不一致时记 `worker_mismatch`，这本身就是要看的信号。
///
/// - `worker_hint` —— JS 侧自称的 worker 名
/// - `method` —— JSON-RPC 方法名（形如 `task_create_task_blocking`）
pub fn record(worker_hint: Option<&str>, method: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let authoritative = CURRENT_WORKER.with(|cell| cell.borrow().clone());
    let (worker, mismatch) = match (authoritative.as_deref(), worker_hint) {
        (Some(real), Some(hint)) if real != hint => (real, Some(hint)),
        (Some(real), _) => (real, None),
        (None, Some(hint)) => (hint, None),
        (None, None) => (UNKNOWN_WORKER, None),
    };
    if let Some(hint) = mismatch {
        // 脚本改了 `__nodeget_current_script_name`：单独告警，不随采样限流丢弃
        warn!(
            target: "js_audit",
            worker,
            hint,
            method = bounded(method),
            "js worker identity mismatch: script reported a different name"
        );
    }

    let now = now_ms();
    let worker = bounded(worker);
    let method = bounded(method);

    let is_first_seen = {
        let mut counters = table().lock().unwrap_or_else(PoisonError::into_inner);
        // 容量上限：新键在表满后折叠进溢出桶，避免脚本用随机方法名撑爆内存
        let key = (worker.clone(), method.clone());
        let key = if counters.len() >= MAX_ENTRIES && !counters.contains_key(&key) {
            (OVERFLOW_WORKER.to_owned(), method.clone())
        } else {
            key
        };
        let entry = counters.entry(key).or_insert(Entry {
            count: 0,
            first_seen_ms: now,
            last_seen_ms: now,
            warned: 0,
        });
        entry.count += 1;
        entry.last_seen_ms = now;
        let first = entry.count == 1;
        // 频率限制：前 3 次都报，之后每 60 次报一次，避免每个 HTTP 请求刷一行 WARN
        let should_warn = entry.count <= 3 || entry.count.is_multiple_of(60);
        if should_warn {
            entry.warned += 1;
        }
        // 显式释放锁：下面的告警日志可能较慢，不应持锁
        drop(counters);
        (first, should_warn)
    };

    let (is_first_seen, should_warn) = is_first_seen;
    let sensitive = is_sensitive(&method);
    if sensitive && should_warn || is_first_seen {
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
        // Handle 未初始化（例如测试环境）：退化为「只在日志里告警、不落盘」。
        // 必须把标志复位，否则这一次失败会让本进程内再也不会尝试启动 flush。
        FLUSH_STARTED.store(false, Ordering::Release);
        warn!(target: "js_audit", error = %e, "js audit flusher not started, will retry");
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
    let mut failed = 0_u32;
    for (worker, payload) in snapshot {
        // 单个 worker 失败不该连累其余 worker（原来 `?` 会让后面的全部跳过）
        if let Err(e) = ng_kv::set_v_to_kv(AUDIT_NAMESPACE.to_owned(), worker, payload).await {
            failed += 1;
            warn!(target: "js_audit", error = %e, "failed to persist js audit row");
        }
    }
    debug!(target: "js_audit", workers, failed, "js audit flushed");
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
    use super::{MAX_ENTRIES, MAX_KEY_CHARS, SENSITIVE_METHODS, is_sensitive, snapshot};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    /// 聚合表与开关都是进程级全局状态，测试之间必须串行，否则互相污染。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

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
        let _guard = lock();
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

    /// 对抗性：脚本篡改 `__nodeget_current_script_name` 嫁祸他人时，
    /// 归属必须仍取 Rust 侧线程本地的真名。
    #[test]
    fn forged_worker_name_does_not_change_attribution() {
        let _guard = lock();
        super::reset_for_test();
        super::ENABLED.store(true, Ordering::Relaxed);

        super::set_current_worker(Some("real-worker"));
        super::record(Some("victim-worker"), "kv_get_value");
        super::clear_current_worker();

        let snap = snapshot();
        assert!(
            snap.iter().any(|(name, _)| name == "real-worker"),
            "必须记到真实执行者名下"
        );
        assert!(
            !snap.iter().any(|(name, _)| name == "victim-worker"),
            "被冒名的 worker 不应出现"
        );
    }

    /// 对抗性：脚本用随机方法名灌内存时必须被容量上限挡住。
    #[test]
    fn audit_table_is_bounded() {
        let _guard = lock();
        super::reset_for_test();
        super::ENABLED.store(true, Ordering::Relaxed);

        for index in 0..(MAX_ENTRIES + 64) {
            super::record(None, &format!("method_{index}"));
        }

        let snap = snapshot();
        assert!(
            snap.len() <= 2,
            "超出容量后新方法必须折叠进溢出桶，实际 worker 桶数：{}",
            snap.len()
        );
        assert!(
            snap.iter().any(|(name, _)| name == super::OVERFLOW_WORKER),
            "溢出桶必须存在，否则等于静默丢数据"
        );
    }

    /// 对抗性：超长方法名不得让日志行/KV key 无界增长。
    #[test]
    fn long_keys_are_truncated() {
        let _guard = lock();
        super::reset_for_test();
        super::ENABLED.store(true, Ordering::Relaxed);

        let long_method = "m".repeat(MAX_KEY_CHARS * 4);
        super::record(None, &long_method);

        let snap = snapshot();
        let methods = snap
            .iter()
            .find(|(name, _)| name == super::UNKNOWN_WORKER)
            .map(|(_, payload)| payload)
            .expect("对未知 worker 也要记录");
        let key = methods
            .get("methods")
            .and_then(|m| m.as_object())
            .and_then(|m| m.keys().next())
            .expect("方法名存在");
        assert!(
            key.chars().count() <= MAX_KEY_CHARS + 16,
            "方法名必须被截断，实际长度 {}",
            key.chars().count()
        );
    }

    /// 敏感名单要覆盖真正危险的方法（含关闭审计自身的 `kv_set_value`）。
    #[test]
    fn sensitive_list_covers_high_risk_methods() {
        for method in [
            "kv_set_value",
            "nodeget-server_self_update",
            "nodeget-server_edit_config",
            "task_delete",
            "static-bucket-file_upload",
        ] {
            assert!(is_sensitive(method), "{method} 必须视为敏感方法");
        }
    }
}
