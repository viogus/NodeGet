//! RBAC 权限模型核心数据结构
//!
//! 定义 Token、Limit、Scope、Permission 等类型，构成 NodeGet 的
//! 基于作用域的权限控制体系。Token 携带多条 Limit，每条 Limit
//! 由作用域（Scope）+ 权限（Permission）组合约束。

use crate::monitoring::query::{DynamicDataQueryField, StaticDataQueryField};
use serde::{Deserialize, Serialize};

/// 已验证的 Token 信息，由鉴权层返回。
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Token {
    /// Token 版本号
    pub version: i32,
    /// Token 的 key 标识
    pub token_key: String,
    /// 有效期起始（Unix 毫秒，None 表示无下界）
    pub timestamp_from: Option<i64>,
    /// 有效期截止（Unix 毫秒，None 表示无上界）
    pub timestamp_to: Option<i64>,
    /// 权限限制列表
    pub token_limit: Vec<Limit>,
    /// 关联的用户名
    pub username: Option<String>,
}

/// 单条权限限制：作用域 + 允许的操作。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Limit {
    /// 此条限制适用的作用域列表
    pub scopes: Vec<Scope>,
    /// 此条限制允许的权限列表
    pub permissions: Vec<Permission>,
}

/// 权限作用域：限定 Token 可操作的资源范围。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// 全局作用域，不限定具体资源
    Global,
    /// 特定 Agent UUID
    AgentUuid(uuid::Uuid),
    /// 特定 KV 命名空间
    KvNamespace(String),
    /// 特定 JS Worker
    JsWorker(String),
    /// 特定静态文件桶
    StaticBucket(String),
    /// 特定数据库连接
    Db(String),
}

/// 权限枚举：每个变体对应一个业务模块的细粒度操作权限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// 静态监控数据权限
    StaticMonitoring(StaticMonitoring),
    /// 动态监控数据权限
    DynamicMonitoring(DynamicMonitoring),
    /// 任务调度权限
    Task(Task),
    /// 定时任务权限
    Crontab(Crontab),
    /// 定时任务执行结果权限
    CrontabResult(CrontabResult),
    /// KV 存储权限
    Kv(Kv),
    /// 终端连接权限
    Terminal(Terminal),
    /// NodeGet 服务级权限
    NodeGet(NodeGet),
    /// 监控 UUID 管理权限
    MonitoringUuid(MonitoringUuid),
    /// JS Worker 权限
    JsWorker(JsWorker),
    /// JS 执行结果权限
    JsResult(JsResult),
    /// 动态监控汇总权限
    DynamicMonitoringSummary(DynamicMonitoringSummary),
    /// 静态文件桶权限
    StaticBucket(StaticBucket),
    /// 静态文件操作权限
    StaticBucketFile(StaticBucketFile),
    /// 数据库连接管理权限
    Db(Db),
}

/// NodeGet 服务级操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeGet {
    /// 列出所有 Agent UUID（已废弃，使用 MonitoringUuid::List）
    #[deprecated(since = "0.2.13", note = "Use MonitoringUuid::List instead")]
    ListAllAgentUuid,
    /// 获取 JS 运行时池状态
    GetRtPool,
    /// 删除 Agent UUID（已废弃，使用 MonitoringUuid::Delete）
    #[deprecated(since = "0.2.13", note = "Use MonitoringUuid::Delete instead")]
    DeleteAgentUuid,
    /// 执行原生 SQL
    ExecSql,
}

/// 监控 UUID 管理权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitoringUuid {
    /// 列出 UUID
    List,
    /// 删除 UUID
    Delete,
}

/// 静态监控数据操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaticMonitoring {
    /// 读取指定字段（附带字段粒度控制）
    Read(StaticDataQueryField),
    /// 上报写入
    Write,
    /// 删除记录
    Delete,
}

/// 动态监控数据操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicMonitoring {
    /// 读取指定字段（附带字段粒度控制）
    Read(DynamicDataQueryField),
    /// 上报写入
    Write,
    /// 删除记录
    Delete,
}

/// 动态监控汇总数据操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicMonitoringSummary {
    /// 读取汇总
    Read,
    /// 上报汇总
    Write,
    /// 删除汇总
    Delete,
}

/// 任务调度操作权限，关联任务类型名
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    /// 创建指定类型的任务
    Create(String),
    /// 读取指定类型的任务
    Read(String),
    /// 修改指定类型的任务
    Write(String),
    /// 删除指定类型的任务
    Delete(String),
    /// 监听任务事件
    Listen,
}

/// 定时任务操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Crontab {
    /// 读取定时任务配置
    Read,
    /// 创建或修改定时任务
    Write,
    /// 删除定时任务
    Delete,
}

/// 定时任务执行结果操作权限，关联 Cron 名称
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrontabResult {
    /// 读取指定 Cron 的执行结果
    Read(String),
    /// 删除指定 Cron 的执行结果
    Delete(String),
}

/// KV 存储操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kv {
    /// 列出所有命名空间
    ListAllNamespace,
    /// 列出所有键
    ListAllKeys,
    /// 读取指定命名空间的值
    Read(String),
    /// 写入指定命名空间的值
    Write(String),
    /// 删除指定命名空间的键
    Delete(String),
}

/// 终端连接权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Terminal {
    /// 建立 WebSocket 终端连接
    Connect,
}

/// JS Worker 操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsWorker {
    /// 列出所有 JS Worker
    ListAllJsWorker,
    /// 创建 JS Worker
    Create,
    /// 读取 JS Worker 配置
    Read,
    /// 修改 JS Worker 配置
    Write,
    /// 删除 JS Worker
    Delete,
    /// 执行已注册的 JS Worker
    RunDefinedJsWorker,
    /// 执行内联 JS 代码
    RunRawJsWorker,
}

/// JS 执行结果操作权限，关联 Worker 名称
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsResult {
    /// 读取指定 Worker 的执行结果
    Read(String),
    /// 删除指定 Worker 的执行结果
    Delete(String),
}

/// 静态文件桶操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaticBucket {
    /// 读取桶信息
    Read,
    /// 创建或修改桶
    Write,
    /// 删除桶
    Delete,
}

/// 静态文件操作权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaticBucketFile {
    /// 读取文件
    Read,
    /// 上传文件
    Write,
    /// 删除文件
    Delete,
    /// 列出文件目录
    List,
}

/// 数据库连接管理权限
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Db {
    /// 列出所有数据库连接
    List,
    /// 读取数据库连接信息
    Read,
    /// 创建数据库连接
    Create,
    /// 更新数据库连接
    Update,
    /// 删除数据库连接
    Delete,
    /// 执行原生 SQL
    ExecSql,
}
