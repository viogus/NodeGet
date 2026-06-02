//! Ping 任务模块。
//!
//! 提供三种 Ping 方式的实现：
//! - `icmp` — ICMP Echo Ping（需 raw socket 或 CAP_NET_RAW 权限）
//! - `tcp` — TCP 连接 Ping（测量 TCP 握手耗时）
//! - `http` — HTTP GET Ping（测量 HTTP 请求往返耗时）

/// HTTP Ping 任务模块
pub mod http;
/// ICMP Ping 任务模块
pub mod icmp;
/// TCP Ping 任务模块
pub mod tcp;
