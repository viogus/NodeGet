#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::similar_names
)]

//! `ng-js-runtime` —— `QuickJS` 运行时池与字节码编译。
//!
//! ## 默认 feature（仅类型）
//! - [`JsCodeInput`] —— JS 代码输入（源码或字节码）
//! - [`RunType`] —— 运行模式枚举（Call/Cron/Route/InlineCall）
//! - [`CompileMode`] —— 编译模式（字节码 vs 源码）
//! - [`RuntimePoolInfo`]、[`RuntimePoolWorkerInfo`] —— 运行时池状态类型
//!
//! ## `server` feature
//! - [`compile_js_module_to_bytecode()`] —— 将 JS 模块编译为字节码
//! - [`runtime_pool`] 模块 —— `QuickJS` 实例的 OS 线程池
//! - [`server_runtime`] 模块 —— 服务器级运行时操作（`spawn_on_server_runtime`）
//! - [`nodeget`] 模块 —— 向 JS 上下文注入 `nodeget()` API
//! - [`inline_call`] 模块 —— 内联 JS 执行
//! - [`js_worker_service`] 模块 —— `JsWorkerService` trait（依赖注入点）
//! - [`RuntimeLimits`] —— 运行时资源限制
//! - [`js_runner`] / [`js_runner_source_mode`] —— 一次性 JS 执行

mod types;

pub use types::*;

// ── 仅 server feature 启用的模块 ───────────────────────────────────

#[cfg(feature = "server")]
pub mod inline_call;
#[cfg(feature = "server")]
pub mod js_worker_service;
#[cfg(feature = "server")]
pub mod nodeget;
#[cfg(feature = "server")]
pub mod runtime_pool;
#[cfg(feature = "server")]
pub mod server_runtime;
#[cfg(feature = "server")]
mod spawn_on_server_runtime;

// ── 仅 server feature 启用的重导出 ────────────────────────────────

#[cfg(feature = "server")]
pub use server_runtime::{
    RuntimeLimits, compile_js_module_to_bytecode, format_js_error, js_error, js_runner,
    js_runner_source_mode,
};
#[cfg(feature = "server")]
pub use spawn_on_server_runtime::{init as init_server_runtime, spawn_on_server_runtime};
