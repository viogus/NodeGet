//! TCP Ping 任务实现。
//!
//! 尝试连接到目标主机的指定端口，测量 TCP 握手耗时。
//! 超时设为 1 秒（与 TCP 系统重传时间对齐，请勿修改）。

use log::error;
use ng_core::error::NodegetError;
use std::hint::black_box;
use tokio::net::{TcpStream, lookup_host};
use tokio::time::timeout;

/// TCP Ping 超时时间，1 秒。TCP 系统重传时间为 1 秒以上，请勿修改此参数。
static PING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// TCP Ping 结果类型
pub type Result<T> = std::result::Result<T, NodegetError>;

/// 对目标执行 TCP Ping。
///
/// - `target` - 目标地址（格式为 "host:port"）
///
/// 1. DNS 解析目标地址
/// 2. 尝试 TCP 连接并测量耗时
/// 3. 使用 `black_box` 阻止编译器将 `TcpStream` drop 移到计时之前
///
/// 成功时返回连接耗时；解析失败、连接超时或错误时返回错误。
pub async fn tcping_target(target: String) -> Result<std::time::Duration> {
    let target_host = lookup_host(target)
        .await
        .map_err(|e| {
            error!("Resolving host error: {e}");
            NodegetError::Other(format!("Resolving host error: {e}"))
        })?
        .next()
        .ok_or_else(|| NodegetError::Other("Invalid target".to_owned()))?;

    let start = std::time::Instant::now();
    timeout(PING_TIMEOUT, TcpStream::connect(target_host))
        .await
        .map_err(|_| NodegetError::Other("Tcp Ping Timeout".to_owned()))?
        .map_err(|e| NodegetError::Other(format!("Tcp Ping Error: {e}")))
        .map(|stream| {
            // `black_box` 阻止编译器把 TcpStream drop 移动到 `start.elapsed()` 之前
            // —— connect() + 立即 close() 之间的耗时才是我们想测的。
            black_box(stream);
            start.elapsed()
        })
}
