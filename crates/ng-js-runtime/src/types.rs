//! JS 运行时核心类型定义。
//!
//! 包含 `RunType`、`CompileMode`、`JsCodeInput` 以及运行时池状态类型，
//! 默认 feature 下即可使用，不依赖 server 专有逻辑。

use serde::{Deserialize, Serialize};

/// JS Worker 的运行模式，决定调用哪个 handler 函数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunType {
    /// 手动调用，对应 `export default { onCall() }`
    Call,
    /// 定时任务调用，对应 `export default { onCron() }`
    Cron,
    /// HTTP 路由调用，对应 `export default { onRoute() }`
    Route,
    /// 内联调用（从另一个 JS Worker 中调用），对应 `export default { onInlineCall() }`
    InlineCall,
}

/// JS 脚本编译模式。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CompileMode {
    /// 使用预编译字节码执行（默认，性能更优）
    #[default]
    Bytecode,
    /// 使用源码模式执行（每次重新解析编译）
    Source,
}

impl RunType {
    /// 返回运行模式的字符串标识，用于序列化和日志。
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Cron => "cron",
            Self::Route => "route",
            Self::InlineCall => "inline_call",
        }
    }

    /// 返回 JS 端对应的 handler 函数名（如 `"onCall"`）。
    #[must_use]
    pub const fn handler_name(&self) -> &'static str {
        match self {
            Self::Call => "onCall",
            Self::Cron => "onCron",
            Self::Route => "onRoute",
            Self::InlineCall => "onInlineCall",
        }
    }
}

/// JS 代码输入形式，区分源码和预编译字节码。
#[derive(Debug, Clone)]
pub enum JsCodeInput {
    /// JS 源码字符串
    Source(String),
    /// `QuickJS` 预编译字节码
    Bytecode(Vec<u8>),
}

/// 单个运行时 Worker 的状态信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePoolWorkerInfo {
    /// Worker 对应的脚本名称
    pub script_name: String,
    /// 当前正在执行的请求数
    pub active_requests: usize,
    /// 上次使用时间（ms，unix epoch）
    pub last_used_ms: i64,
    /// 空闲时长（ms）
    pub idle_ms: i64,
    /// 运行时清理时间阈值（ms），None 表示永不清理
    pub runtime_clean_time_ms: Option<i64>,
}

/// 运行时池整体状态信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePoolInfo {
    /// Worker 总数
    pub total_workers: usize,
    /// 各 Worker 的详细状态列表
    pub workers: Vec<RuntimePoolWorkerInfo>,
}
