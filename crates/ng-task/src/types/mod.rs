//! 任务类型定义模块：包含任务事件类型、参数结构体和结果结构体
//!
//! 核心类型：
//! - [`TaskEventType`] — 任务类型枚举，每种类型对应一种可执行操作
//! - [`TaskEvent`] — 任务事件，包含 task_id、task_token 和任务类型
//! - [`TaskEventResult`] — 任务执行结果
//! - [`TaskEventResponse`] — Agent 上传的任务响应结构

// 任务查询模块
pub mod query;

use ng_core::utils::version::NodeGetVersion;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

/// WebShell 任务参数
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct WebShellTask {
    /// WebSocket URL
    pub url: url::Url,
    /// 终端连接 ID（由任务创建方生成的随机 UUID）
    pub terminal_id: uuid::Uuid,
}

/// Execute 任务参数
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct ExecuteTask {
    /// 可执行文件名或路径
    pub cmd: String,
    /// 传递给 cmd 的参数列表
    pub args: Vec<String>,
}

/// HTTP 请求任务参数
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct HttpRequestTask {
    /// 请求 URL
    pub url: url::Url,
    /// 请求方法，如 GET/POST/PUT
    pub method: String,
    /// 请求头（键值对）
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// UTF-8 文本请求体，与 body_base64 互斥
    pub body: Option<String>,
    /// Base64 编码请求体，与 body 互斥
    pub body_base64: Option<String>,
    /// 出口 IP，可传具体 IP 或 "ipv4 auto"/"ipv6 auto"
    pub ip: Option<String>,
}

/// DNS 记录类型枚举
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsRecordType {
    /// IPv4 地址记录
    A,
    /// IPv6 地址记录
    Aaaa,
    /// 别名记录
    Cname,
    /// 邮件交换记录
    Mx,
    /// 文本记录
    Txt,
    /// 域名服务器记录
    Ns,
    /// 服务记录
    Srv,
    /// 指针记录（反向 DNS）
    Ptr,
    /// 起始授权记录
    Soa,
    /// 证书颁发机构授权记录
    Caa,
}

/// DNS 任务参数
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct DnsTask {
    /// 查询域名
    pub domain: String,
    /// 查询记录类型列表
    pub record_types: Vec<DnsRecordType>,
    /// 自定义 DNS 服务器，格式 "IP:port"；None 使用系统默认
    pub dns_server: Option<String>,
}

/// DNS 查询结果
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct DnsRecordResult {
    /// 记录类型
    pub record_type: DnsRecordType,
    /// 解析耗时（毫秒）
    pub time: f64,
    /// 记录数据字符串
    pub data: String,
}

/// HTTP 请求任务结果
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct HttpRequestTaskResult {
    /// HTTP 状态码
    pub status: u16,
    /// 响应头（数组格式，允许重复 key）
    pub headers: Vec<BTreeMap<String, String>>,
    /// UTF-8 文本响应体，与 body_base64 互斥
    pub body: Option<String>,
    /// Base64 编码响应体，与 body 互斥
    pub body_base64: Option<String>,
}

/// 任务事件类型枚举，定义各种可执行的任务类型
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventType {
    /// Ping 任务，参数可能为域名需 DNS 解析
    Ping(String),
    /// TCP Ping 任务，参数可能为域名需 DNS 解析
    TcpPing(String),
    /// HTTP Ping 任务，使用 URL
    HttpPing(url::Url),

    /// Web Shell 任务
    WebShell(WebShellTask),
    /// 命令执行任务，结构化参数（cmd + args）
    Execute(ExecuteTask),
    /// HTTP 请求任务
    HttpRequest(HttpRequestTask),
    /// DNS 查询任务
    Dns(DnsTask),

    /// 读取 Agent 配置任务
    ReadConfig,
    /// 编辑 Agent 配置任务，参数为配置内容
    EditConfig(String),

    /// IP 获取任务，返回 IPv4/IPv6 地址
    Ip,

    /// 获取 Agent 版本信息任务
    Version,

    /// 自我更新任务，参数为更新 tag
    SelfUpdate(String),
}

impl TaskEventType {
    /// 获取任务类型的名称标识符
    #[must_use]
    pub const fn task_name(&self) -> &'static str {
        match self {
            Self::Ping(_) => "ping",
            Self::TcpPing(_) => "tcp_ping",
            Self::HttpPing(_) => "http_ping",
            Self::WebShell(_) => "web_shell",
            Self::Execute(_) => "execute",
            Self::HttpRequest(_) => "http_request",
            Self::Dns(_) => "dns",
            Self::EditConfig(_) => "edit_config",
            Self::ReadConfig => "read_config",
            Self::Ip => "ip",
            Self::Version => "version",
            Self::SelfUpdate(_) => "self_update",
        }
    }

    /// 从延迟创建对应的结果类型
    /// 用于 Ping/TcpPing/HttpPing 任务
    ///
    /// 其他任务类型返回 `None`
    pub fn result_from_duration(&self, duration: Duration) -> Option<TaskEventResult> {
        let millis = duration.as_secs_f64() * 1000.0;
        match self {
            Self::Ping(_) => Some(TaskEventResult::Ping(millis)),
            Self::TcpPing(_) => Some(TaskEventResult::TcpPing(millis)),
            Self::HttpPing(_) => Some(TaskEventResult::HttpPing(millis)),
            _ => None,
        }
    }

    /// 检查任务类型是否为延迟测试类任务
    #[must_use]
    pub const fn is_ping_task(&self) -> bool {
        matches!(self, Self::Ping(_) | Self::TcpPing(_) | Self::HttpPing(_))
    }

    /// 获取任务的权限检查字段名
    /// 用于 Agent 配置中的权限字段匹配
    #[must_use]
    pub const fn permission_field(&self) -> &'static str {
        match self {
            Self::Ping(_) => "allow_icmp_ping",
            Self::TcpPing(_) => "allow_tcp_ping",
            Self::HttpPing(_) => "allow_http_ping",
            Self::WebShell(_) => "allow_web_shell",
            Self::Execute(_) => "allow_execute",
            Self::HttpRequest(_) => "allow_http_request",
            Self::Dns(_) => "allow_dns",
            Self::ReadConfig => "allow_read_config",
            Self::EditConfig(_) => "allow_edit_config",
            Self::Ip => "allow_ip",
            Self::Version => "allow_version",
            Self::SelfUpdate(_) => "allow_self_update",
        }
    }
}

/// 任务事件结构体，定义单个待执行任务的详细信息
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    /// 任务 ID，由数据库自增生成
    pub task_id: u64,
    /// 任务令牌，仅用于校验上传者身份，不参与鉴权
    pub task_token: String,
    /// 任务事件类型及其参数
    pub task_event_type: TaskEventType,
}

/// 任务事件结果枚举，定义任务执行后的返回结果
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventResult {
    /// Ping 结果，单位：毫秒
    Ping(f64),
    /// TCP Ping 结果，单位：毫秒
    TcpPing(f64),
    /// HTTP Ping 结果，单位：毫秒
    HttpPing(f64),

    /// Web Shell 结果，是否连接成功
    WebShell(bool),
    /// 命令执行结果，命令标准输出
    Execute(String),
    /// HTTP 请求结果
    HttpRequest(HttpRequestTaskResult),
    /// DNS 查询结果，可能包含多条记录
    Dns(Vec<DnsRecordResult>),

    /// 读取 Agent 配置结果，返回配置内容字符串
    ReadConfig(String),
    /// 编辑 Agent 配置结果，是否成功
    EditConfig(bool),

    /// IP 获取结果，(IPv4, IPv6)
    Ip(Option<Ipv4Addr>, Option<Ipv6Addr>),

    /// Agent 版本信息结果
    Version(NodeGetVersion),

    /// 自我更新结果，是否成功
    SelfUpdate(bool),
}

impl TaskEventResult {
    /// 获取结果类型对应的任务名称
    #[must_use]
    pub const fn task_name(&self) -> &'static str {
        match self {
            Self::Ping(_) => "ping",
            Self::TcpPing(_) => "tcp_ping",
            Self::HttpPing(_) => "http_ping",
            Self::WebShell(_) => "web_shell",
            Self::Execute(_) => "execute",
            Self::HttpRequest(_) => "http_request",
            Self::Dns(_) => "dns",
            Self::ReadConfig(_) => "read_config",
            Self::EditConfig(_) => "edit_config",
            Self::Ip(_, _) => "ip",
            Self::Version(_) => "version",
            Self::SelfUpdate(_) => "self_update",
        }
    }

    /// 从延迟创建结果（用于 Ping/TcpPing/HttpPing）
    #[must_use]
    pub const fn from_duration(task_type: &TaskEventType, duration: Duration) -> Option<Self> {
        match task_type {
            TaskEventType::Ping(_) => Some(Self::Ping(duration.as_secs_f64() * 1000.0)),
            TaskEventType::TcpPing(_) => Some(Self::TcpPing(duration.as_secs_f64() * 1000.0)),
            TaskEventType::HttpPing(_) => Some(Self::HttpPing(duration.as_secs_f64() * 1000.0)),
            _ => None,
        }
    }
}

/// 任务事件响应结构体，Agent 上传任务执行结果时使用
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct TaskEventResponse {
    /// 任务 ID
    pub task_id: u64,
    /// Agent 的 UUID
    pub agent_uuid: uuid::Uuid,
    /// 任务令牌，用于校验上传者身份
    pub task_token: String,
    /// 完成时间戳（毫秒）
    pub timestamp: u64,

    /// 执行是否成功
    pub success: bool,

    /// 错误消息，执行失败时填写
    pub error_message: Option<String>,
    /// 任务事件结果，成功时填写
    pub task_event_result: Option<TaskEventResult>,
}
