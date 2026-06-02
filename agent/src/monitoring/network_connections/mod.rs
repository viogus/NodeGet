//! 网络连接统计模块。
//!
//! 提供跨平台的 UDP/TCP 连接数统计功能：
//! - Linux：通过 Netlink `SOCK_DIAG` 接口查询（子模块 `netlink`）
//! - Windows：通过 `netstat2` 库枚举
//! - 其他平台：返回 (0, 0) 占位

// Linux 平台特定的网络连接统计实现
#[cfg(target_os = "linux")]
mod netlink;

/// 计算网络连接数（Linux 平台）。
///
/// 通过 Netlink SOCK_DIAG 分别查询 IPv4/IPv6 的 UDP/TCP 连接数并汇总。
///
/// 返回元组 `(UDP 连接数, TCP 连接数)`。
#[cfg(target_os = "linux")]
pub fn calc_connections() -> (u64, u64) {
    // (udp, tcp)
    use netlink::connections_count_with_protocol;
    let tcp4 =
        connections_count_with_protocol(libc::AF_INET as u8, libc::IPPROTO_TCP as u8).unwrap_or(0);
    let tcp6 =
        connections_count_with_protocol(libc::AF_INET6 as u8, libc::IPPROTO_TCP as u8).unwrap_or(0);
    let udp4 =
        connections_count_with_protocol(libc::AF_INET as u8, libc::IPPROTO_UDP as u8).unwrap_or(0);
    let udp6 =
        connections_count_with_protocol(libc::AF_INET6 as u8, libc::IPPROTO_UDP as u8).unwrap_or(0);
    (udp4 + udp6, tcp4 + tcp6)
}

/// 计算网络连接数（Windows 平台）。
///
/// 使用 `netstat2` 库枚举当前系统的 UDP 和 TCP 连接数。
///
/// 返回元组 `(UDP 连接数, TCP 连接数)`。
#[cfg(target_os = "windows")]
pub fn calc_connections() -> (u64, u64) {
    use netstat2::{ProtocolFlags, ProtocolSocketInfo, iterate_sockets_info_without_pids};

    iterate_sockets_info_without_pids(ProtocolFlags::TCP | ProtocolFlags::UDP)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .fold((0, 0), |(udp, tcp), info| match info.protocol_socket_info {
            ProtocolSocketInfo::Tcp(_) => (udp, tcp + 1),
            ProtocolSocketInfo::Udp(_) => (udp + 1, tcp),
        })
}

/// 计算网络连接数（其他平台）。
///
/// 目前尚未支持 macOS 等平台，返回零值占位。
///
/// 返回元组 `(0, 0)`。
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub const fn calc_connections() -> (u64, u64) {
    (0, 0) // TODO: MacOS Support
}
