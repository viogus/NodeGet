//! Server 配置文件结构体与解析逻辑。
//!
//! 定义 `ServerConfig`、`DatabaseConfig`、`LoggingConfig`、`MonitoringBufferConfig` 等类型，
//! 以及配置读取与 auto_gen UUID 替换方法。

use crate::config::deserialize_uuid_or_auto;
use crate::config::replace_auto_gen_uuid;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

/// 默认 Docker 隧道 socket 目录（D1-A：每个 agent 一个 unix socket）
pub const DEFAULT_DOCKER_TUNNEL_SOCKET_DIR: &str = "/run/nodeget/docker";
/// 默认 Docker 隧道 socket 文件权限（八进制字符串，仅属主可读写）
pub const DEFAULT_DOCKER_TUNNEL_SOCKET_FILE_MODE: &str = "0600";
/// 默认 Docker 隧道空闲回收时间（秒）
pub const DEFAULT_DOCKER_TUNNEL_IDLE_TIMEOUT_SECS: u64 = 300;
/// 默认单个 agent 的并发隧道上限
pub const DEFAULT_DOCKER_TUNNEL_MAX_TUNNELS: usize = 4;
/// 默认 Docker 隧道 TCP 监听地址（D1-B：跨机形态；默认只监听回环）
pub const DEFAULT_DOCKER_TUNNEL_TCP_LISTEN: &str = "127.0.0.1:23750";
/// 默认 Docker 隧道 TCP 入口的路径前缀（D1-B）
pub const DEFAULT_DOCKER_TUNNEL_PATH_PREFIX: &str = "/agent";

/// 服务器配置结构体，定义 Server 的运行参数。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerConfig {
    /// Server UUID，默认自动生成
    #[serde(deserialize_with = "deserialize_uuid_or_auto")]
    pub server_uuid: uuid::Uuid,

    /// WebSocket 监听地址（如 `0.0.0.0:3000`）
    pub ws_listener: String,

    /// JSON-RPC 最大并发连接数，默认 100
    pub jsonrpc_max_connections: Option<u32>,

    /// 是否启用 Unix Socket（仅非 Windows 平台）
    pub enable_unix_socket: Option<bool>,

    /// Unix Socket 路径（仅非 Windows 平台，默认 `/var/lib/nodeget.sock`）
    pub unix_socket_path: Option<String>,

    /// 日志配置（可选，不填则使用默认值）
    pub logging: Option<LoggingConfig>,

    /// 数据库连接配置
    pub database: DatabaseConfig,

    /// 监控数据缓冲写入配置（可选）
    pub monitoring_buffer: Option<MonitoringBufferConfig>,

    /// 数据库保留期清理配置（可选，不填等价于 dry-run）
    pub retention: Option<RetentionConfig>,

    /// crontab 派发行为配置（可选）
    pub crontab: Option<CrontabConfig>,

    /// Docker 隧道配置（可选，默认关闭）
    pub docker_tunnel: Option<DockerTunnelConfig>,

    /// JSON-RPC 最大请求体大小（字节），默认 10485760（10MB）
    pub max_request_body_size: Option<u32>,

    /// JSON-RPC 最大响应体大小（字节），默认 104857600（100MB）
    pub max_response_body_size: Option<u32>,

    /// TLS 证书文件路径（PEM 格式），必须与 `tls_key` 同时指定才启用 TLS
    pub tls_cert: Option<String>,

    /// TLS 私钥文件路径（PEM 格式），必须与 `tls_cert` 同时指定才启用 TLS
    pub tls_key: Option<String>,

    /// 静态文件服务根目录，默认 `./static/`
    pub static_path: Option<String>,

    /// 本地 SQLite 数据库存放目录，默认 `./db/`
    pub db_path: Option<String>,
}

/// 监控数据缓冲写入配置。
///
/// 控制 `report_static`、`report_dynamic`、`report_dynamic_summary` 三个写入接口
/// 的批量插入行为。启用后，数据先缓存在内存中，按间隔批量写入数据库。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MonitoringBufferConfig {
    /// 刷新间隔（毫秒），默认 500
    pub flush_interval_ms: Option<u64>,
    /// 单次最大批量大小，默认 1000
    pub max_batch_size: Option<usize>,
    /// Channel 容量，默认 10000
    pub channel_capacity: Option<usize>,
}

/// 数据库保留期清理配置。
///
/// 清理 `task` / `crontab_result` / `js_result` 三张表的过期数据。保留期取值仍然由
/// 数据库内的 `database_limit_*`（UI 可改）决定，本配置只控制「是否真正删除」与执行节奏。
///
/// 默认 `enabled = false`，即 dry-run：只统计并打日志、不删除任何行，便于先核对数量。
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RetentionConfig {
    /// 是否真正执行删除，默认 `false`（dry-run，只打日志）
    pub enabled: Option<bool>,
    /// 清理间隔（秒），默认 3600
    pub interval_secs: Option<u64>,
    /// 启动后首次清理的延迟（秒），默认 30
    pub initial_delay_secs: Option<u64>,
    /// 历史行（`created_at IS NULL`）的 id 安全边界，默认 100000
    pub legacy_id_margin: Option<i64>,
}

/// crontab 派发行为配置。
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CrontabConfig {
    /// `crontab_result` 的成功行是否按「每次运行一条汇总行」写入，默认 `true`。
    ///
    /// 置 `false` 回退为「每个 Agent 一条成功行」。失败行始终逐个 Agent 记录。
    pub aggregate_success_result: Option<bool>,
}

/// Docker 隧道配置（D1-A：每个启用隧道的 agent 分配一个 unix socket）。
///
/// Docker socket 等价于宿主机 root 权限，因此默认关闭，必须显式 `enable = true`
/// 并在 `agents` 中逐个列出允许的 agent UUID。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DockerTunnelConfig {
    /// 总开关，默认 `false`
    pub enable: Option<bool>,

    /// unix socket 目录，默认 `/run/nodeget/docker`；
    /// 每个启用隧道的 agent 对应 `<socket_dir>/<agent_uuid>.sock`
    pub socket_dir: Option<String>,

    /// D1-B：TCP 监听地址（跨机形态），默认不监听；
    /// 显式填写才启用，例如 `"0.0.0.0:23750"`（跨机必须监听非回环地址）
    pub tcp_listen: Option<String>,

    /// D1-B：TCP 入口的路径前缀，默认 `/agent`；
    /// 端点形态为 `tcp://<host>:<port><path_prefix>/<agent_uuid>`
    pub path_prefix: Option<String>,

    /// D1-B：TCP 入口是否要求 `Authorization: Bearer <NodeGet token>`，默认 `true`
    pub require_token: Option<bool>,

    /// 允许建立隧道的 Agent UUID 列表；为空表示不启用任何 agent
    pub agents: Option<Vec<String>>,

    /// socket 文件权限（八进制字符串），默认 `"0600"`
    pub socket_file_mode: Option<String>,

    /// socket 文件属主 UID（仅在以 root 运行时生效，`None` 表示不改属主）
    pub socket_file_uid: Option<u32>,

    /// socket 文件属组 GID（仅在以 root 运行时生效，`None` 表示不改属组）
    pub socket_file_gid: Option<u32>,

    /// 单条隧道连接的读写空闲回收时间（秒），默认 300；`0` 表示不回收
    pub idle_timeout_secs: Option<u64>,

    /// 单个 agent 的并发隧道上限，默认 4；`0` 表示不限制
    pub max_tunnels: Option<usize>,
}

impl DockerTunnelConfig {
    /// 是否启用 Docker 隧道，默认 `false`
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enable.unwrap_or(false)
    }

    /// 获取 socket 目录，未配置时使用默认值（/run/nodeget/docker）
    #[must_use]
    pub fn socket_dir_or_default(&self) -> &str {
        self.socket_dir
            .as_deref()
            .unwrap_or(DEFAULT_DOCKER_TUNNEL_SOCKET_DIR)
    }

    /// 获取 D1-B 的 TCP 监听地址；未配置或为空视为**不启用** TCP 入口
    #[must_use]
    pub fn tcp_listen(&self) -> Option<&str> {
        self.tcp_listen
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// 获取 D1-B 的路径前缀，未配置时使用默认值（`/agent`）。
    ///
    /// 结果已归一化：以 `/` 开头、不以 `/` 结尾；配置成 `/` 时返回空串，
    /// 表示端点就是 `tcp://<host>:<port>/<agent_uuid>`。
    #[must_use]
    pub fn path_prefix_or_default(&self) -> String {
        let raw = self
            .path_prefix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_DOCKER_TUNNEL_PATH_PREFIX);
        let trimmed = raw.trim_end_matches('/');
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed.starts_with('/') {
            trimmed.to_owned()
        } else {
            format!("/{trimmed}")
        }
    }

    /// D1-B 的 TCP 入口是否要求 Bearer Token，默认 `true`。
    ///
    /// 置 `false` 只应在「入口本身已被防火墙/私网限制」时使用：TCP 端点等价于
    /// 目标主机 root，未鉴权即等于把该主机的 Docker 控制权公开出去。
    #[must_use]
    pub fn require_token_or_default(&self) -> bool {
        self.require_token.unwrap_or(true)
    }

    /// 获取 socket 文件权限位，未配置或格式非法时使用默认值（0600）
    #[must_use]
    pub fn socket_file_mode_or_default(&self) -> u32 {
        self.socket_file_mode
            .as_deref()
            .and_then(parse_socket_file_mode)
            .unwrap_or(0o600)
    }

    /// 获取空闲回收时间（秒），未配置时使用默认值（300）
    #[must_use]
    pub fn idle_timeout_secs_or_default(&self) -> u64 {
        self.idle_timeout_secs
            .unwrap_or(DEFAULT_DOCKER_TUNNEL_IDLE_TIMEOUT_SECS)
    }

    /// 获取单个 agent 的并发隧道上限，未配置时使用默认值（4）
    #[must_use]
    pub fn max_tunnels_or_default(&self) -> usize {
        self.max_tunnels
            .unwrap_or(DEFAULT_DOCKER_TUNNEL_MAX_TUNNELS)
    }

    /// 获取允许建立隧道的 agent UUID 列表（去重，忽略空白项与非法 UUID）
    #[must_use]
    pub fn agent_uuids(&self) -> Vec<uuid::Uuid> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for raw in self.agents.iter().flatten() {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            match uuid::Uuid::parse_str(trimmed) {
                Ok(uuid) => {
                    if seen.insert(uuid) {
                        out.push(uuid);
                    }
                }
                Err(_) => {
                    tracing::warn!(value = %trimmed, "docker_tunnel.agents 中存在非法 UUID，已忽略");
                }
            }
        }
        out
    }
}

/// 解析八进制 socket 权限字符串，支持 `"0600"` 与 `"0o600"` 两种写法。
fn parse_socket_file_mode(raw: &str) -> Option<u32> {
    let digits = raw.trim();
    let digits = digits.strip_prefix("0o").unwrap_or(digits);
    u32::from_str_radix(digits, 8).ok()
}

/// 日志配置。
///
/// `log_filter` / `json_log_filter` 的语法与 `RUST_LOG` 环境变量一致，
/// 例如 `"info,kv=debug,monitoring=trace,db=warn"`。
///
/// 可用的 target：
///   `server`、`rpc`、`db`、`kv`、`monitoring`、`task`、`token`、
///   `js_worker`、`js_result`、`crontab`、`crontab_result`、
///   `js_runtime`、`terminal`
///
/// 虚拟 target `db` 会自动展开为
/// `sea_orm=<level>,sea_orm_migration=<level>,sqlx=<level>`。
///
/// 如果设置了 `RUST_LOG` 环境变量，它会覆盖 `log_filter`。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LoggingConfig {
    /// 控制台日志过滤器，语法同 `RUST_LOG`，默认 `"info"`
    pub log_filter: Option<String>,

    /// JSON 日志输出文件路径（可选，不设置则不输出 JSON 日志）
    pub json_log_file: Option<String>,

    /// JSON 日志过滤器，语法同 `RUST_LOG`（可选，默认与 `log_filter` 相同）
    pub json_log_filter: Option<String>,

    /// 内存日志缓冲区容量（条数），默认 500，设为 0 表示禁用内存日志
    pub memory_log_capacity: Option<usize>,

    /// 内存日志过滤器，语法同 `RUST_LOG`（可选，默认与 `log_filter` 相同）
    /// 通过 `nodeget-server_log` RPC 方法可查询缓冲区内容
    pub memory_log_filter: Option<String>,
}

/// 数据库配置结构体，定义数据库连接参数。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DatabaseConfig {
    /// 数据库连接 URL
    pub database_url: String,
    /// 数据库连接超时时间（毫秒）
    pub connect_timeout_ms: Option<u64>,
    /// 获取连接超时时间（毫秒）
    pub acquire_timeout_ms: Option<u64>,
    /// 连接空闲超时时间（毫秒）
    pub idle_timeout_ms: Option<u64>,
    /// 连接最大生存时间（毫秒）
    pub max_lifetime_ms: Option<u64>,
    /// 最大连接数
    pub max_connections: Option<u32>,
    /// TimescaleDB 时序优化配置（可选，仅对安装了 timescaledb 扩展的 PostgreSQL 生效）
    pub timescale: Option<ng_core::config::TimescaleConfig>,
}

impl ServerConfig {
    /// 从指定路径读取并解析服务器配置。
    ///
    /// 若配置文件中 `server_uuid` 为 `"auto_gen"`，则会生成随机 UUIDv4
    /// 并直接覆盖原配置文件，后续启动不再触发自动生成。
    ///
    /// - `path` — 配置文件路径
    /// - 返回解析后的 `ServerConfig`
    ///
    /// 内部步骤：
    /// 1. 读取配置文件内容
    /// 2. 检查 `server_uuid` 是否为 `auto_gen`，若是则生成 UUID 并原子写回
    /// 3. 解析 TOML 为 `ServerConfig`
    ///
    /// # Errors
    ///
    /// 当文件读取失败或 TOML 解析失败时返回错误
    pub async fn get_and_parse_config(
        path: impl AsRef<Path>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // 1. 读取配置文件内容
        let content = fs::read_to_string(path.as_ref()).await?;

        // 2. 检查并替换 auto_gen
        let value: toml::Value = toml::from_str(&content)?;
        let is_auto_gen = value
            .get("server_uuid")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("auto_gen"));

        if is_auto_gen {
            let new_uuid = uuid::Uuid::new_v4().to_string();
            let new_content = replace_auto_gen_uuid(&content, "server_uuid", &new_uuid);

            // 原子写入：先写 .tmp，成功后 rename，避免崩溃导致配置文件损坏
            let tmp_path = path.as_ref().with_extension("tmp");
            fs::write(&tmp_path, &new_content).await?;
            fs::rename(&tmp_path, path.as_ref()).await?;

            // 3. 解析替换后的配置
            let config: Self = toml::from_str(&new_content)?;
            return Ok(config);
        }

        // 3. 解析原始配置（无需替换）
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_config_minimal_toml() {
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.server_uuid,
            uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
        assert_eq!(config.ws_listener, "0.0.0.0:3000");
        assert_eq!(config.database.database_url, "sqlite://./db/nodeget.db");
        assert!(config.jsonrpc_max_connections.is_none());
        assert!(config.enable_unix_socket.is_none());
        assert!(config.logging.is_none());
        assert!(config.monitoring_buffer.is_none());
        assert!(config.retention.is_none());
        assert!(config.crontab.is_none());
        assert!(config.max_request_body_size.is_none());
        assert!(config.max_response_body_size.is_none());
        assert!(config.tls_cert.is_none());
        assert!(config.tls_key.is_none());
        assert!(config.static_path.is_none());
        assert!(config.db_path.is_none());
    }

    #[test]
    fn server_config_full_toml() {
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"
jsonrpc_max_connections = 200
enable_unix_socket = true
unix_socket_path = "/tmp/nodeget.sock"
max_request_body_size = 5242880
max_response_body_size = 52428800
tls_cert = "/etc/tls/cert.pem"
tls_key = "/etc/tls/key.pem"
static_path = "./files/"
db_path = "./data/"

[database]
database_url = "postgres://user:pass@localhost/nodeget"
connect_timeout_ms = 5000
acquire_timeout_ms = 10000
idle_timeout_ms = 600000
max_lifetime_ms = 1800000
max_connections = 20

[logging]
log_filter = "info,db=warn"
json_log_file = "/var/log/nodeget.json"
json_log_filter = "debug"
memory_log_capacity = 1000
memory_log_filter = "trace"

[monitoring_buffer]
flush_interval_ms = 1000
max_batch_size = 500
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.jsonrpc_max_connections, Some(200));
        assert_eq!(config.enable_unix_socket, Some(true));
        assert_eq!(
            config.unix_socket_path,
            Some("/tmp/nodeget.sock".to_owned())
        );
        assert_eq!(config.max_request_body_size, Some(5242880));
        assert_eq!(config.max_response_body_size, Some(52428800));
        assert_eq!(config.tls_cert, Some("/etc/tls/cert.pem".to_owned()));
        assert_eq!(config.tls_key, Some("/etc/tls/key.pem".to_owned()));
        assert_eq!(config.static_path, Some("./files/".to_owned()));
        assert_eq!(config.db_path, Some("./data/".to_owned()));

        let db = &config.database;
        assert_eq!(db.database_url, "postgres://user:pass@localhost/nodeget");
        assert_eq!(db.connect_timeout_ms, Some(5000));
        assert_eq!(db.acquire_timeout_ms, Some(10000));
        assert_eq!(db.idle_timeout_ms, Some(600000));
        assert_eq!(db.max_lifetime_ms, Some(1800000));
        assert_eq!(db.max_connections, Some(20));

        let logging = config.logging.unwrap();
        assert_eq!(logging.log_filter, Some("info,db=warn".to_owned()));
        assert_eq!(
            logging.json_log_file,
            Some("/var/log/nodeget.json".to_owned())
        );
        assert_eq!(logging.json_log_filter, Some("debug".to_owned()));
        assert_eq!(logging.memory_log_capacity, Some(1000));
        assert_eq!(logging.memory_log_filter, Some("trace".to_owned()));

        let buffer = config.monitoring_buffer.unwrap();
        assert_eq!(buffer.flush_interval_ms, Some(1000));
        assert_eq!(buffer.max_batch_size, Some(500));
    }

    #[test]
    fn monitoring_buffer_config_defaults() {
        let toml_str = r#"
[monitoring_buffer]
"#;
        let config: MonitoringBufferConfig = toml::from_str(toml_str).unwrap();
        assert!(config.flush_interval_ms.is_none());
        assert!(config.max_batch_size.is_none());
    }

    #[test]
    fn retention_config_defaults_to_dry_run() {
        let config = RetentionConfig::default();
        assert!(config.enabled.is_none());
        assert!(config.interval_secs.is_none());
        assert!(config.initial_delay_secs.is_none());
        assert!(config.legacy_id_margin.is_none());
    }

    #[test]
    fn server_config_parses_retention_and_crontab_sections() {
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"

[retention]
enabled = true
interval_secs = 1800

[crontab]
aggregate_success_result = false
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();

        let retention = config.retention.unwrap();
        assert_eq!(retention.enabled, Some(true));
        assert_eq!(retention.interval_secs, Some(1800));
        assert!(retention.initial_delay_secs.is_none());
        assert!(retention.legacy_id_margin.is_none());

        assert_eq!(
            config.crontab.unwrap().aggregate_success_result,
            Some(false)
        );
    }

    #[test]
    fn database_config_required_fields() {
        let toml_str = r#"
database_url = "sqlite://./db/nodeget.db"
"#;
        let config: DatabaseConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.database_url, "sqlite://./db/nodeget.db");
        assert!(config.connect_timeout_ms.is_none());
        assert!(config.max_connections.is_none());
    }

    #[test]
    fn logging_config_partial() {
        let toml_str = r#"
log_filter = "debug"
"#;
        let config: LoggingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.log_filter, Some("debug".to_owned()));
        assert!(config.json_log_file.is_none());
        assert!(config.json_log_filter.is_none());
        assert!(config.memory_log_capacity.is_none());
        assert!(config.memory_log_filter.is_none());
    }

    #[test]
    fn server_config_missing_database_fails() {
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"
"#;
        let result: Result<ServerConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn server_config_missing_ws_listener_fails() {
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"

[database]
database_url = "sqlite://./db/nodeget.db"
"#;
        let result: Result<ServerConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn docker_tunnel_config_defaults_and_parsing() {
        // 未配置时：关闭、默认目录/权限/超时/上限
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert!(config.docker_tunnel.is_none());

        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"

[docker_tunnel]
enable = true
socket_dir = "/tmp/nodeget-docker"
agents = [
    "550e8400-e29b-41d4-a716-446655440001",
    "550e8400-e29b-41d4-a716-446655440001",
    "  550e8400-e29b-41d4-a716-446655440002  ",
    "not-a-uuid",
]
socket_file_mode = "0640"
socket_file_uid = 1000
socket_file_gid = 1001
idle_timeout_secs = 60
max_tunnels = 0
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        let tunnel = config.docker_tunnel.unwrap();
        assert!(tunnel.enabled());
        assert_eq!(tunnel.socket_dir_or_default(), "/tmp/nodeget-docker");
        assert_eq!(tunnel.socket_file_mode_or_default(), 0o640);
        assert_eq!(tunnel.idle_timeout_secs_or_default(), 60);
        assert_eq!(tunnel.max_tunnels_or_default(), 0);
        assert_eq!(tunnel.socket_file_uid, Some(1000));
        assert_eq!(tunnel.socket_file_gid, Some(1001));
        // 非法 UUID 被忽略，重复项被去重
        assert_eq!(tunnel.agent_uuids().len(), 2);
    }

    #[test]
    fn docker_tunnel_config_tcp_defaults_and_parsing() {
        // 未配置 tcp_listen：不启用 D1-B，其余取默认值
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"

[docker_tunnel]
enable = true
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        let tunnel = config.docker_tunnel.unwrap();
        assert!(tunnel.tcp_listen().is_none());
        assert_eq!(tunnel.path_prefix_or_default(), "/agent");
        assert!(tunnel.require_token_or_default());

        // 显式配置；路径前缀做归一化（补前导斜杠、去尾斜杠）
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"

[docker_tunnel]
enable = true
tcp_listen = "0.0.0.0:23750"
path_prefix = "nodeget/docker/"
require_token = false
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        let tunnel = config.docker_tunnel.unwrap();
        assert_eq!(tunnel.tcp_listen(), Some("0.0.0.0:23750"));
        assert_eq!(tunnel.path_prefix_or_default(), "/nodeget/docker");
        assert!(!tunnel.require_token_or_default());

        // 空白 tcp_listen 等价于未配置；`/` 前缀等价于空前缀
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"

[docker_tunnel]
enable = true
tcp_listen = "   "
path_prefix = "/"
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        let tunnel = config.docker_tunnel.unwrap();
        assert!(tunnel.tcp_listen().is_none());
        assert_eq!(tunnel.path_prefix_or_default(), "");
    }

    #[test]
    fn docker_tunnel_config_parses_0o_prefix_mode() {
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"

[docker_tunnel]
socket_file_mode = "0o600"
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        let tunnel = config.docker_tunnel.unwrap();
        assert!(!tunnel.enabled());
        assert_eq!(tunnel.socket_file_mode_or_default(), 0o600);
        assert_eq!(
            tunnel.socket_dir_or_default(),
            DEFAULT_DOCKER_TUNNEL_SOCKET_DIR
        );
        assert_eq!(
            tunnel.idle_timeout_secs_or_default(),
            DEFAULT_DOCKER_TUNNEL_IDLE_TIMEOUT_SECS
        );
        assert_eq!(
            tunnel.max_tunnels_or_default(),
            DEFAULT_DOCKER_TUNNEL_MAX_TUNNELS
        );
    }

    #[test]
    fn server_config_roundtrip_json() {
        // Use serde_json for roundtrip since toml::to_string needs the "display" feature
        let toml_str = r#"
server_uuid = "550e8400-e29b-41d4-a716-446655440000"
ws_listener = "0.0.0.0:3000"

[database]
database_url = "sqlite://./db/nodeget.db"
"#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.server_uuid, deserialized.server_uuid);
        assert_eq!(config.ws_listener, deserialized.ws_listener);
        assert_eq!(
            config.database.database_url,
            deserialized.database.database_url
        );
    }
}
