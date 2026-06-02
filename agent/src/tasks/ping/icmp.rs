//! ICMP Ping 任务实现。
//!
//! 使用 `surge_ping` 库发送 ICMP Echo 请求并测量往返耗时。
//! IPv4 和 IPv6 各维护一个全局客户端单例，内部使用 Arc 共享 socket，无需 Mutex。

use log::error;
use ng_core::error::NodegetError;
use rand::random;
use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence, SurgeError};
use tokio::net::lookup_host;
use tokio::sync::OnceCell;

/// ICMP Ping 结果类型
pub type Result<T> = std::result::Result<T, NodegetError>;

/// ICMP Ping 负载数据（8 字节零填充）。
static ICMP_PAYLOAD: [u8; 8] = [0; 8];
/// Ping 超时时间，2 秒。
static PING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// 全局 IPv4 ICMP 客户端单例。
/// Client 内部使用 `Arc` 共享 socket 和 `recv_task`，Clone 即可并发使用，无需 Mutex。
static GLOBAL_ICMP_V4_CLIENT: OnceCell<Client> = OnceCell::const_new();
/// 全局 IPv6 ICMP 客户端单例。
static GLOBAL_ICMP_V6_CLIENT: OnceCell<Client> = OnceCell::const_new();

/// 获取 IPv4 ICMP 客户端单例。
async fn get_v4_client() -> &'static Client {
    GLOBAL_ICMP_V4_CLIENT
        .get_or_init(|| async {
            let config = Config::builder().kind(ICMP::V4).build();
            Client::new(&config).unwrap()
        })
        .await
}

/// 获取 IPv6 ICMP 客户端单例。
async fn get_v6_client() -> &'static Client {
    GLOBAL_ICMP_V6_CLIENT
        .get_or_init(|| async {
            let config = Config::builder().kind(ICMP::V6).build();
            Client::new(&config).unwrap()
        })
        .await
}

/// 对目标 IP 执行 ICMP Ping。
///
/// - `target` - 目标 IP 地址（IPv4 或 IPv6）
///
/// 返回往返时间；ICMP 请求失败时返回 `SurgeError`。
async fn ping_ip(target: std::net::IpAddr) -> std::result::Result<std::time::Duration, SurgeError> {
    let client = if target.is_ipv4() {
        get_v4_client().await
    } else {
        get_v6_client().await
    };

    let mut pinger = client.pinger(target, PingIdentifier(random())).await;
    pinger.timeout(PING_TIMEOUT);

    let (_, duration) = pinger.ping(PingSequence(random()), &ICMP_PAYLOAD).await?;

    Ok(duration)
}

/// 对目标执行 ICMP Ping（支持域名）。
///
/// - `target` - 目标地址（可以是 IP 或域名）
///
/// 1. 若目标为 IP 则直接使用
/// 2. 若目标为域名则进行 DNS 查询（带 `PING_TIMEOUT` 硬超时）
/// 3. 根据 IP 版本选择 IPv4/IPv6 客户端发送 ICMP Echo
///
/// 成功时返回往返时间；解析或 Ping 失败时返回错误。
pub async fn ping_target(target: String) -> Result<std::time::Duration> {
    // DNS lookup under a hard timeout: a hung system resolver must not
    // stall every ICMP ping. 2s matches PING_TIMEOUT so total latency
    // stays bounded.
    let target_ip = match target.parse::<std::net::IpAddr>() {
        Ok(ip) => Some(ip),
        Err(_) => {
            match tokio::time::timeout(PING_TIMEOUT, lookup_host(format!("{}:{}", target, 80)))
                .await
            {
                Ok(Ok(mut addrs)) => addrs.next().map(|e| e.ip()),
                Ok(Err(e)) => {
                    error!("Resolving host error: {e}");
                    None
                }
                Err(_) => {
                    error!("Resolving host timed out after {}s", PING_TIMEOUT.as_secs());
                    None
                }
            }
        }
    };

    let Some(target) = target_ip else {
        return Err(NodegetError::Other("Invalid target".to_owned()));
    };

    ping_ip(target)
        .await
        .map_err(|e| NodegetError::Other(format!("{e}")))
}
