//! 定时任务核心类型定义：Cron、CronType、AgentCronType、ServerCronType。
//!
//! 这些类型在默认 feature 下即可用，Agent 端和 Server 端共享。

use ng_task::TaskEventType;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// 定时任务定义，对应数据库 crontab 表的一条记录。
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Cron {
    /// 定时任务 ID（数据库自增主键）
    pub id: i64,
    /// 定时任务名称（全局唯一标识）
    pub name: String,
    /// 是否启用
    pub enable: bool,
    /// Cron 表达式（如 "*/5 * * * *"）
    pub cron_expression: String,
    /// 定时任务类型（Agent 端 / Server 端）
    pub cron_type: CronType,
    /// 上次运行时间（毫秒时间戳），None 表示从未运行
    pub last_run_time: Option<i64>,
}

/// 定时任务类型枚举，区分 Agent 端下发和 Server 端本地执行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronType {
    /// Agent 端定时任务：向指定 UUID 列表的 Agent 下发任务
    Agent(Vec<Uuid>, AgentCronType),
    /// Server 端定时任务：在本地执行
    Server(ServerCronType),
}

/// Agent 端定时任务子类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCronType {
    /// 向 Agent 下发指定类型的任务事件
    Task(TaskEventType),
}

/// Server 端定时任务子类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerCronType {
    /// JS Worker 脚本执行：第一个字段为脚本名，第二个字段为传入参数
    JsWorker(String, Value),
}
