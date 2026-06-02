//! NTP 时间偏移获取模块。
//!
//! 启动时向配置的 NTP 服务器查询本地时间与参考时间的偏差，
//! 校准结果通过 [`ng_core::utils::set_ntp_offset_ms`] 写入全局偏移，
//! 供后续时间戳生成使用。连接失败或超时时回退到本地时间（偏移为 0）。

use log::{info, warn};
use sntpc::{NtpContext, NtpTimestampGenerator, get_time};
use sntpc_net_tokio::UdpSocketWrapper;
use tokio::net::UdpSocket;
use tokio::time::{Duration, timeout};

/// NTP 协议默认端口。
const DEFAULT_NTP_PORT: u16 = 123;
/// 单次 NTP 请求超时时间。
const NTP_TIMEOUT: Duration = Duration::from_secs(10);

/// 基于 `SystemTime` 的 NTP 时间戳生成器，供 sntpc 库使用。
#[derive(Copy, Clone, Default)]
struct StdTimestampGen {
    /// 距 Unix Epoch 的时长，`init()` 时设置。
    duration: Option<std::time::Duration>,
}

impl NtpTimestampGenerator for StdTimestampGen {
    fn init(&mut self) {
        self.duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok();
    }

    fn timestamp_sec(&self) -> u64 {
        self.duration.map_or(0, |d| d.as_secs())
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        self.duration.map_or(0, |d| d.subsec_micros())
    }
}

/// 从指定的 NTP 服务器获取本地时间与 NTP 参考时间的偏差（毫秒）。
///
/// - `ntp_server` - NTP 服务器主机名或 IP 地址
///
/// 连接失败或超时时返回 0，等同于使用本地时间。
pub async fn fetch_ntp_offset(ntp_server: &str) -> i64 {
    let Some(addr) = resolve_ntp_addr(ntp_server).await else {
        warn!(
            "Failed to resolve NTP server address for: {ntp_server}; falling back to local time (offset=0)"
        );
        return 0;
    };

    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => UdpSocketWrapper::from(s),
        Err(e) => {
            warn!("Failed to bind UDP socket for NTP: {e}; falling back to local time (offset=0)");
            return 0;
        }
    };

    let context = NtpContext::new(StdTimestampGen::default());
    let result = timeout(NTP_TIMEOUT, get_time(addr, &socket, context)).await;

    match result {
        Ok(Ok(time)) => {
            let offset_us = time.offset();
            // 有符号整数除法在 Rust 中对负数向 0 截断，例如 -1999 / 1000 = -1 而非 -2。
            // NTP offset 在局域网里经常就是 ±几十 us 这种小数量级，直接整除会把符号丢掉并且
            // 偏离真实值。用 f64 round 做四舍五入到最近的 ms，再转回 i64。
            #[allow(clippy::cast_possible_truncation)]
            let offset_ms = (offset_us as f64 / 1000.0).round() as i64;
            info!(
                "NTP sync success: server={ntp_server}, offset={offset_ms} ms (raw={offset_us} us)"
            );
            offset_ms
        }
        Ok(Err(e)) => {
            warn!(
                "NTP request failed for {ntp_server}: {e:?}; falling back to local time (offset=0)"
            );
            0
        }
        Err(_) => {
            warn!(
                "NTP request timed out after 10s for {ntp_server}; falling back to local time (offset=0)"
            );
            0
        }
    }
}

/// 将 NTP 服务器地址解析为 `SocketAddr`（默认端口 123）。
///
/// - `server` - NTP 服务器主机名或 IP 地址
///
/// 返回第一个解析结果；解析失败返回 `None`。
async fn resolve_ntp_addr(server: &str) -> Option<std::net::SocketAddr> {
    let with_port = format!("{server}:{DEFAULT_NTP_PORT}");
    let mut addrs = tokio::net::lookup_host(&with_port).await.ok()?;
    addrs.next()
}
