//! CrontabResult 类型定义：定时任务执行结果的数据结构。
//!
//! 对应数据库 crontab_result 表，记录每次定时任务触发的执行结果。

/// 定时任务执行结果记录，对应 crontab_result 表的一条数据。
#[derive(Debug, Clone)]
pub struct CrontabResult {
    /// 记录 ID（数据库自增主键）
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
