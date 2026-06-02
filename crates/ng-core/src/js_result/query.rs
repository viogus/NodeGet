//! JS Worker 执行结果查询条件与查询请求体

use serde::{Deserialize, Serialize};

/// JS 执行结果的单条查询条件，多条件组合使用。
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsResultQueryCondition {
    /// 按结果 ID 精确匹配
    Id(i64),
    /// 按 JS Worker ID 匹配
    JsWorkerId(i64),
    /// 按 JS Worker 名称匹配
    JsWorkerName(String),
    /// 按运行类型（cron / manual 等）匹配
    RunType(String),
    /// 开始时间范围：[from, to]（Unix 毫秒）
    StartTimeFromTo(i64, i64),
    /// 开始时间下界（Unix 毫秒）
    StartTimeFrom(i64),
    /// 开始时间上界（Unix 毫秒）
    StartTimeTo(i64),
    /// 结束时间范围：[from, to]（Unix 毫秒）
    FinishTimeFromTo(i64, i64),
    /// 结束时间下界（Unix 毫秒）
    FinishTimeFrom(i64),
    /// 结束时间上界（Unix 毫秒）
    FinishTimeTo(i64),
    /// 仅返回执行成功的结果
    IsSuccess,
    /// 仅返回执行失败的结果
    IsFailure,
    /// 仅返回正在运行的结果
    IsRunning,
    /// 限制返回行数
    Limit(u64),
    /// 仅返回最新一条
    Last,
}

/// JS 执行结果查询请求体，包含多条件组合。
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsResultDataQuery {
    /// 查询条件列表，各条件之间为 AND 关系
    pub condition: Vec<JsResultQueryCondition>,
}
