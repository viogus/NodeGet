//! `get_uuid` 子命令
//!
//! 输出服务器的 UUID 到标准输出，供运维脚本等场景使用。

/// 输出服务器 UUID
///
/// - config：已解析的服务器配置（包含 `server_uuid` 字段）
pub fn run(config: &ng_config::config::server::ServerConfig) {
    println!("{}", config.server_uuid);
}
