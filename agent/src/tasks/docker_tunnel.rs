//! Docker 隧道任务模块（设计文档 Phase 1 / D1-A）。
//!
//! Agent 侧只做两件事：用自身 `server.ws_url` 派生出 Server 的 `/docker-tunnel`
//! 端点并主动拨出 WebSocket；然后把本地 Docker socket 与该 WebSocket 之间的字节
//! 双向搬运。**全程不解析 HTTP**——Server 侧同样是裸字节对拷，因此 Docker API 的
//! keep-alive、chunked、hijack（attach/exec）都天然可用。
//!
//! 任务载荷里不带 URL：Server 并不知道 Agent 可达的地址，而 Agent 用来连上
//! Server 的 `ws_url` 本身就是这条可达地址，只需把 path 换成 `/docker-tunnel`。

use crate::AGENT_CONFIG;
use futures_util::{SinkExt, StreamExt};
use log::info;
use ng_config::config::agent::Server;
use ng_core::error::NodegetError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio_tungstenite::tungstenite::protocol::Message;
use url::Url;

/// 结果类型
pub type Result<T> = std::result::Result<T, NodegetError>;

/// WebSocket 握手超时时间，避免异常 Server 让任务永久挂起。
const DOCKER_TUNNEL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 单次搬运的缓冲区大小（Docker API 报文通常远小于此值）。
const TUNNEL_BUFFER_SIZE: usize = 32 * 1024;

/// 当前活跃的 Docker 隧道数。
static ACTIVE_TUNNELS: AtomicUsize = AtomicUsize::new(0);

/// 活跃隧道计数守卫，`Drop` 时递减计数。
struct TunnelGuard;

impl TunnelGuard {
    /// 尝试占用一个隧道名额。
    ///
    /// `max_sessions` 为 `0` 表示不限制；超过上限时立即失败（不排队，避免堆积）。
    fn acquire(max_sessions: usize) -> Result<Self> {
        if max_sessions == 0 {
            ACTIVE_TUNNELS.fetch_add(1, Ordering::AcqRel);
            return Ok(Self);
        }

        // compare_exchange 循环保证并发下不会超卖名额
        let mut current = ACTIVE_TUNNELS.load(Ordering::Acquire);
        loop {
            if current >= max_sessions {
                return Err(NodegetError::Other(format!(
                    "Docker tunnel session limit reached (max {max_sessions} concurrent tunnels)"
                )));
            }
            match ACTIVE_TUNNELS.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self),
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for TunnelGuard {
    fn drop(&mut self) {
        ACTIVE_TUNNELS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 读取 Agent 自身配置中的 Docker socket 路径与并发上限。
fn tunnel_settings() -> Result<(String, usize)> {
    let config = AGENT_CONFIG
        .get()
        .ok_or_else(|| NodegetError::Other("Agent config not initialized".to_owned()))?
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok((
        config.docker_tunnel_socket_path_or_default().to_owned(),
        config.docker_tunnel_max_sessions_or_default(),
    ))
}

/// 由 Agent 自身的 `server.ws_url` 派生 Server 的 Docker 隧道端点。
///
/// 保留原 scheme/host/port，把 path 替换为 `/docker-tunnel`，并带上
/// `agent_uuid` / `task_id` / `task_token` 三个查询参数（Server 侧据此反查任务表）。
///
/// 返回派生后的 URL；`ws_url` 非法或 Agent 配置未初始化时返回错误字符串。
pub fn build_tunnel_url(
    server: &Server,
    task_id: u64,
    task_token: &str,
) -> std::result::Result<Url, String> {
    let mut url = Url::parse(&server.ws_url).map_err(|e| format!("Invalid ws_url: {e}"))?;

    let scheme = url.scheme().to_owned();
    if !((scheme == "ws") || (scheme == "wss")) {
        return Err(format!("Invalid scheme: {scheme}"));
    }
    if url.host_str().is_none() {
        return Err(format!("Invalid host: {url}"));
    }

    let agent_uuid = AGENT_CONFIG
        .get()
        .ok_or("Agent config not initialized")?
        .read()
        .map_err(|_| "Agent Config lock poisoned")?
        .agent_uuid;

    url.set_path("/docker-tunnel");
    {
        let mut serializer = url.query_pairs_mut();
        serializer.clear();
        serializer.append_pair("agent_uuid", &agent_uuid.to_string());
        serializer.append_pair("task_id", &task_id.to_string());
        serializer.append_pair("task_token", task_token);
    }

    Ok(url)
}

/// 执行一次 Docker 隧道任务：拨出 WebSocket 并把 Docker socket 与它双向对接。
///
/// - `url` - [`build_tunnel_url`] 的结果
/// - `ignore_cert` - 是否跳过 TLS 证书校验
///
/// 返回隧道结束（任一侧关闭）的结果；连接失败时返回错误。
#[cfg(unix)]
pub async fn handle_docker_tunnel_url(
    url: std::result::Result<Url, String>,
    ignore_cert: bool,
) -> Result<()> {
    // build_tunnel_url 用 String 报错（它不依赖 anyhow），这里并入本模块的错误类型
    let url = url.map_err(NodegetError::Other)?;
    let (socket_path, max_sessions) = tunnel_settings()?;
    let _guard = TunnelGuard::acquire(max_sessions)?;

    // 先连本地 socket：本地就不可用时不必让 Server 侧登记一次注定失败的 pending 项
    let stream = tokio::net::UnixStream::connect(&socket_path)
        .await
        .map_err(|e| {
            NodegetError::Other(format!(
                "Failed to connect to Docker socket {socket_path}: {e}"
            ))
        })?;

    let connector = crate::rpc::multi_server::build_connector(ignore_cert);
    let ws_stream = match tokio::time::timeout(
        DOCKER_TUNNEL_CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_tls_with_config(url.to_string(), None, false, connector),
    )
    .await
    {
        Ok(Ok(ws)) => ws.0,
        Ok(Err(e)) => {
            return Err(NodegetError::AgentConnectionError(format!(
                "Failed to connect to WebSocket: {e}"
            )));
        }
        Err(_) => {
            return Err(NodegetError::AgentConnectionError(format!(
                "WebSocket connect timed out after {}s",
                DOCKER_TUNNEL_CONNECT_TIMEOUT.as_secs()
            )));
        }
    };

    // 日志里绝不出现 task_token：隧道 URL 的 query 携带任务令牌，只能记脱敏后的形态
    let log_url = redacted_tunnel_url(&url);

    info!("Docker tunnel established: {socket_path} <-> {log_url}");

    let result = pump_bytes(stream, ws_stream).await;

    info!("Docker tunnel closed: {socket_path} <-> {log_url}");

    result
}

/// 把隧道 URL 的 query 里的 `task_token` 替换为占位符，供日志使用。
fn redacted_tunnel_url(url: &Url) -> String {
    let mut redacted = url.clone();
    let pairs: Vec<(String, String)> = redacted
        .query_pairs()
        .map(|(key, value)| {
            let value = if key == "task_token" {
                "***REDACTED***".to_owned()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();
    redacted.query_pairs_mut().clear().extend_pairs(pairs);
    redacted.to_string()
}

/// 非 Unix 平台（Windows 使用 named pipe 而非 unix socket）暂不支持。
#[cfg(not(unix))]
pub async fn handle_docker_tunnel_url(
    _url: std::result::Result<Url, String>,
    _ignore_cert: bool,
) -> Result<()> {
    Err(NodegetError::Other(
        "Docker tunnel is only supported on Unix platforms".to_owned(),
    ))
}

/// 半关闭控制帧：某一方向不再有数据时发送，对端据此只关闭 socket 的写半边。
///
/// 数据一律走 `Binary`，`Text` 专用于控制帧。`docker run`/`attach`/`exec` 会先关闭
/// 本地连接的写半边（表示不再有 stdin），若当成整条连接结束，daemon 之后写回的
/// 输出就会丢失。
const HALF_CLOSE_CONTROL: &str = "{\"t\":\"half_close\"}";

/// 在 unix socket 与 WebSocket 之间双向搬运字节。
///
/// 单个方向的 EOF 只做半关闭，不拆掉整条隧道；任一侧出错或对端关闭时返回。
#[cfg(unix)]
async fn pump_bytes<S>(
    stream: tokio::net::UnixStream,
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut ws_sink, mut ws_source) = ws_stream.split();
    let (mut sock_reader, mut sock_writer) = stream.into_split();
    let mut buf = vec![0u8; TUNNEL_BUFFER_SIZE];

    let mut local_done = false;
    let mut remote_done = false;

    loop {
        if local_done && remote_done {
            let _ = ws_sink.close().await;
            return Ok(());
        }

        tokio::select! {
            biased;

            // 本地 Docker 客户端 → Server
            read = sock_reader.read(&mut buf), if !local_done => {
                let n = read.map_err(|e| {
                    NodegetError::Other(format!("Failed to read from Docker socket: {e}"))
                })?;
                if n == 0 {
                    local_done = true;
                    ws_sink
                        .send(Message::Text(HALF_CLOSE_CONTROL.into()))
                        .await
                        .map_err(|e| {
                            NodegetError::AgentConnectionError(format!(
                                "Failed to send half-close control frame: {e}"
                            ))
                        })?;
                } else {
                    ws_sink
                        .send(Message::Binary(
                            tokio_tungstenite::tungstenite::Bytes::copy_from_slice(&buf[..n]),
                        ))
                        .await
                        .map_err(|e| {
                            NodegetError::AgentConnectionError(format!("Failed to send to WebSocket: {e}"))
                        })?;
                }
            }

            // Server → 本地 Docker socket
            message = ws_source.next(), if !remote_done => {
                match message {
                    Some(Ok(Message::Binary(data))) => {
                        sock_writer.write_all(&data).await.map_err(|e| {
                            NodegetError::Other(format!("Failed to write to Docker socket: {e}"))
                        })?;
                    }
                    Some(Ok(Message::Text(text))) => {
                        if text.as_str() == HALF_CLOSE_CONTROL {
                            remote_done = true;
                            let _ = sock_writer.shutdown().await;
                        } else {
                            sock_writer.write_all(text.as_bytes()).await.map_err(|e| {
                                NodegetError::Other(format!("Failed to write to Docker socket: {e}"))
                            })?;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        return Err(NodegetError::AgentConnectionError(format!(
                            "Failed to read from WebSocket: {e}"
                        )));
                    }
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn redacted_tunnel_url_hides_task_token() {
        let url = Url::parse(
            "ws://127.0.0.1:2211/docker-tunnel?agent_uuid=a&task_id=7&task_token=secret",
        )
        .unwrap();
        let redacted = redacted_tunnel_url(&url);
        assert!(redacted.contains("task_token=***REDACTED***"), "{redacted}");
        assert!(!redacted.contains("secret"), "{redacted}");
        assert!(redacted.contains("task_id=7"), "{redacted}");
    }

    fn server_config(ws_url: &str) -> Server {
        Server {
            name: "test".to_owned(),
            server_uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
            token: "token".to_owned(),
            ws_url: ws_url.to_owned(),
            allow_task: None,
            allow_icmp_ping: None,
            allow_tcp_ping: None,
            allow_http_ping: None,
            allow_web_shell: None,
            allow_read_config: None,
            allow_edit_config: None,
            allow_execute: None,
            allow_http_request: None,
            allow_docker_tunnel: None,
            allow_ip: None,
            allow_dns: None,
            allow_version: None,
            allow_self_update: None,
            ignore_cert: None,
            allow_task_type: None,
        }
    }

    #[test]
    fn build_tunnel_url_replaces_path_and_adds_query() {
        // agent_uuid 取自全局 AGENT_CONFIG，它是否已初始化取决于测试执行顺序，
        // 因此对成功与失败两种结果都做校验，避免顺序相关的 flaky 测试。
        let server = server_config("ws://127.0.0.1:2211/nodeget/rpc");
        match build_tunnel_url(&server, 7, "abc") {
            Ok(url) => {
                assert_eq!(url.scheme(), "ws");
                assert_eq!(url.path(), "/docker-tunnel");
                let query: std::collections::HashMap<_, _> =
                    url.query_pairs().into_owned().collect();
                assert_eq!(query.get("task_id").map(String::as_str), Some("7"));
                assert_eq!(query.get("task_token").map(String::as_str), Some("abc"));
                assert!(
                    query.contains_key("agent_uuid"),
                    "missing agent_uuid: {url}"
                );
            }
            Err(e) => assert!(
                e.contains("Agent config not initialized"),
                "unexpected error: {e}"
            ),
        }
    }

    #[test]
    fn build_tunnel_url_rejects_invalid_scheme() {
        let server = server_config("http://127.0.0.1:2211/nodeget/rpc");
        let err = build_tunnel_url(&server, 7, "abc").unwrap_err();
        assert!(err.contains("Invalid scheme"), "unexpected error: {err}");
    }

    #[test]
    fn tunnel_guard_enforces_max_sessions() {
        ACTIVE_TUNNELS.store(0, Ordering::Release);

        let first = TunnelGuard::acquire(1).unwrap();
        assert!(TunnelGuard::acquire(1).is_err());
        drop(first);

        // 释放后可以重新占用
        let second = TunnelGuard::acquire(1).unwrap();
        assert_eq!(ACTIVE_TUNNELS.load(Ordering::Acquire), 1);
        drop(second);
        assert_eq!(ACTIVE_TUNNELS.load(Ordering::Acquire), 0);

        // 0 表示不限制
        let unlimited = TunnelGuard::acquire(0).unwrap();
        assert_eq!(ACTIVE_TUNNELS.load(Ordering::Acquire), 1);
        drop(unlimited);
        assert_eq!(ACTIVE_TUNNELS.load(Ordering::Acquire), 0);
    }
}
