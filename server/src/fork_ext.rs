//! fork 专属的启动注入点，集中存放本 fork 在服务启动流程里的挂钩。
//!
//! 本仓库是 NodeSeekDev/NodeGet 的 fork，比上游多出几项功能：crontab 派发策略、
//! 数据库保留期清理 worker。这些功能都要嵌进 [`crate::subcommands::serve`] 的启动流程，
//! 而上游的 `serve.rs` 一直在重构（初始化顺序、路由拼装、收尾流程都改过），
//! 把 hook 直接写在那里，每次同步上游都要在同一批位置反复解决冲突。
//!
//! 所以把 fork 的注入点收敛到本模块，`serve.rs` 里只留一行调用：
//! - [`inject_rpc_policies`]：注册策略，必须在 `get_modules()` 之前
//! - [`spawn_background_workers`]：拉起后台任务
//!
//! 以后新增 fork 功能优先加在这里，不要直接改 `serve.rs`。

use ng_config::config::server::ServerConfig;
use tracing::{debug, warn};

/// 注册 fork 的 RPC 策略。
///
/// 必须在 [`crate::rpc_nodeget::get_modules`] 之前调用：策略是全局状态，
/// 注册晚了第一轮请求会读到默认值。
///
/// 当前注册的策略：
/// - crontab 派发策略：成功结果是否按「每次运行一条汇总行」写入 `crontab_result`
///
/// - `config`：本轮使用的服务器配置
pub fn inject_rpc_policies(config: &ServerConfig) {
    let aggregate_success_result = config
        .crontab
        .as_ref()
        .and_then(|crontab| crontab.aggregate_success_result)
        .unwrap_or(true);
    ng_crontab::set_dispatch_policy(ng_crontab::DispatchPolicy {
        aggregate_success_result,
    });
    debug!(
        target: "server",
        aggregate_success_result,
        "ng-crontab dispatch policy registered"
    );
}

/// 拉起 fork 的后台任务。
///
/// 每轮 `serve` 启动都会调用一次，配置热重载时相当于重新拉起，因此这里拉起的
/// 后台任务必须能安全地重复启动。
///
/// 当前拉起的后台任务：
/// - 保留期清理 worker（默认 dry-run，只统计并打日志）
///
/// - `config`：本轮使用的服务器配置
pub fn spawn_background_workers(config: &ServerConfig) {
    spawn_retention_worker(config);
}

/// 拉起数据库保留期清理 worker。
///
/// 默认 dry-run：只统计并打 `warn!` 日志，不删任何行；在 config.toml 里设置
/// `[retention] enabled = true` 后才真正删除。
///
/// 数据库未初始化时只告警不作为错误：清理是可选能力，不该因为它挡住房服务启动。
///
/// - `config`：本轮使用的服务器配置
fn spawn_retention_worker(config: &ServerConfig) {
    let Some(db) = ng_db::get_db() else {
        warn!(
            target: "server",
            "database connection not initialized, retention sweeper not started"
        );
        return;
    };

    let retention = config.retention.clone().unwrap_or_default();
    let options = ng_task::retention::RetentionOptions {
        enabled: retention.enabled.unwrap_or(false),
        interval: std::time::Duration::from_secs(
            retention
                .interval_secs
                .unwrap_or(ng_task::retention::DEFAULT_INTERVAL_SECS),
        ),
        initial_delay: std::time::Duration::from_secs(
            retention
                .initial_delay_secs
                .unwrap_or(ng_task::retention::DEFAULT_INITIAL_DELAY_SECS),
        ),
        legacy_id_margin: retention
            .legacy_id_margin
            .unwrap_or(ng_task::retention::DEFAULT_LEGACY_ID_MARGIN),
    };
    ng_task::retention::spawn_worker(db, options);
}
