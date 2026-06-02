//! CrontabResult 查询 DSL：定义查询条件和响应结构。
//!
//! `CrontabResultQueryCondition` 提供链式过滤条件，
//! `CrontabResultDataQuery` 将条件列表封装为 RPC 请求参数，
//! `CrontabResultResponseItem` 为 RPC 响应中的单条结果项。

use serde::{Deserialize, Serialize};

/// CrontabResult 查询条件枚举，支持多种过滤维度。
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrontabResultQueryCondition {
    /// 按记录 ID 过滤
    Id(i64),
    /// 按 cron_id 过滤
    CronId(i64),
    /// 按 cron_name 过滤
    CronName(String),
    /// 按运行时间范围过滤（起始毫秒时间戳，结束毫秒时间戳）
    RunTimeFromTo(i64, i64),
    /// 按运行时间起始点过滤（毫秒时间戳）
    RunTimeFrom(i64),
    /// 按运行时间结束点过滤（毫秒时间戳）
    RunTimeTo(i64),
    /// 仅查找成功的记录（success 字段为 true）
    IsSuccess,
    /// 仅查找失败的记录（success 字段为 false）
    IsFailure,
    /// 限制返回结果数量
    Limit(u64),
    /// 获取最后一条记录（按 run_time 降序取首条）
    Last,
}

/// CrontabResult 数据查询结构体，封装条件列表作为 RPC 请求参数。
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrontabResultDataQuery {
    /// 查询条件列表，各条件之间为 AND 关系
    pub condition: Vec<CrontabResultQueryCondition>,
}

/// CrontabResult RPC 响应项，对应单条执行结果记录。
#[derive(Serialize)]
pub struct CrontabResultResponseItem {
    /// 记录 ID
    pub id: i64,
    /// 所属定时任务 ID
    pub cron_id: i64,
    /// 所属定时任务名称
    pub cron_name: String,
    /// 关联资源 ID（Agent 任务 ID 或 JS Worker 运行 ID）
    pub relative_id: Option<i64>,
    /// 运行时间（毫秒时间戳）
    pub run_time: Option<i64>,
    /// 是否执行成功
    pub success: Option<bool>,
    /// 执行结果消息
    pub message: Option<String>,
}
