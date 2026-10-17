//! Docker 隧道服务端模块（设计文档 Phase 1 / D1-A）。
//!
//! 职责：为每个启用了隧道的 Agent 在本机绑定一个 unix socket
//!（`<socket_dir>/<agent_uuid>.sock`），Arcane 等管理端把这个 socket 当成本机
//! Docker daemon 使用；Server 侧不解析任何 HTTP，只把 unix socket 与 Agent 拨入的
//! WebSocket 之间的字节原样搬运，因此 Docker API 的 keep-alive、chunked、
//! hijack（attach/exec）都天然可用。
//!
//! 连接流程：
//! 1. 本地管理端 connect 到 `<agent_uuid>.sock`
//! 2. Server 接受连接，登记到待认领表，并向该 Agent 下发一个 `DockerTunnel` 任务
//! 3. Agent 用任务里的 `task_token` 反连 `/docker-tunnel` WebSocket
//! 4. Server 校验任务归属与类型后，把 socket 与 WebSocket 对接，开始裸字节对拷
//!
//! 鉴权：unix socket 文件权限（socket 等价于宿主机 root，必须限制属主/属组）。
//!
//! D1-B（跨机纳管，设计文档 Phase 2）：额外提供可选的 TCP 入口
//!（`[docker_tunnel] tcp_listen`）。Docker 客户端把
//! `<tcp_listen><path_prefix>/<agent_uuid>` 当作 Docker endpoint
//!（如 `tcp://nodeget:23750/agent/<uuid>`）。moby 客户端会把 host URL 的 path
//! 当作 basePath 加到**每一个**请求上（keep-alive 的第 2、3 个请求同样带前缀），
//! 因此这里不解析 HTTP 报文，而是用 hyper 的 HTTP/1.1 编解码器逐请求剥掉前缀后
//! 走隧道转发；101（attach/exec 的 hijack）用 `hyper::upgrade` 原样透传。
//! TCP 入口要求 `Authorization: Bearer <token>`（需 `Task::Create("docker_tunnel")`
//! 权限），该头只用于 `NodeGet` 自身校验，转发前会被删除。

use axum::body::Bytes;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket};
use axum::extract::{Query, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use ng_config::config::server::{DockerTunnelConfig, ServerConfig};
use ng_core::error::{NodegetError, anyhow_to_nodeget_error};
use ng_core::permission::data_structure::{Permission, Scope, Task};
use ng_core::permission::token_auth::TokenOrAuth;
use ng_core::utils::error_message::generate_error_message;
use ng_db::entity::task;
use ng_task::{TaskEventType, TaskManager};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Deserialize;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// 单次字节搬运的缓冲区大小
const TUNNEL_BUFFER_SIZE: usize = 32 * 1024;

/// 等待 Agent 认领一条已 accept 的本地连接的最长时间
const PENDING_CLAIM_TIMEOUT: Duration = Duration::from_mins(1);

/// Agent 认领后交给搬运任务的东西。
struct ClaimedTunnel {
    /// Agent 拨入的 WebSocket
    socket: WebSocket,
    /// 该连接的并发名额，持有到隧道结束才释放
    permit: Option<OwnedSemaphorePermit>,
}

/// 待认领的隧道条目。
struct PendingTunnel {
    /// Agent 拨入后通过它把 WebSocket 交给搬运任务
    sender: oneshot::Sender<ClaimedTunnel>,
    /// 该连接的并发名额，认领时转移给搬运任务
    permit: Option<OwnedSemaphorePermit>,
}

/// 全局待认领隧道表，key 为 `(agent_uuid, task_id)`。
static PENDING_TUNNELS: LazyLock<Mutex<HashMap<(Uuid, u64), PendingTunnel>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 当前 accept 循环与清理任务的句柄，停止时统一 abort。
static RUNTIME_HANDLES: LazyLock<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// 当前已绑定的 socket 文件路径，停止时清理。
static SOCKET_PATHS: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// `/docker-tunnel` 的查询参数（由 Agent 拨入时携带）。
#[derive(Deserialize)]
struct TunnelParams {
    /// Agent UUID
    agent_uuid: String,
    /// 任务 ID
    task_id: u64,
    /// 任务令牌，用于验证请求来源
    task_token: String,
}

/// 每个 Agent 的监听循环所需参数。
struct AgentTunnelRuntime {
    /// Agent UUID
    agent_uuid: Uuid,
    /// socket 文件路径（仅用于日志）
    socket_path: PathBuf,
    /// 单条连接的空闲回收时间（`None` 表示不回收）
    idle_timeout: Option<Duration>,
    /// 并发隧道名额（`None` 表示不限制）
    semaphore: Option<Arc<Semaphore>>,
}

/// 注册 `/docker-tunnel` 路由。
///
/// 调用方必须把它 `.merge()` 到主 Router 的 `.fallback()` 之前。
pub fn router() -> axum::Router {
    axum::Router::new().route(
        "/docker-tunnel",
        axum::routing::get(docker_tunnel_ws_handler),
    )
}

/// 按配置启动所有 Agent 的 Docker 隧道监听器。
///
/// 幂等：重复调用会先 [`stop`] 再重建，因此可直接挂在配置热重载流程里。
pub fn start(config: &ServerConfig) {
    stop();

    let Some(tunnel) = config.docker_tunnel.as_ref() else {
        return;
    };
    if !tunnel.enabled() {
        debug!(target: "server", "docker_tunnel 未启用，跳过");
        return;
    }

    let agents = tunnel.agent_uuids();
    if agents.is_empty() {
        warn!(target: "server", "docker_tunnel 已启用但未配置任何 agents，跳过");
        return;
    }

    let socket_dir = PathBuf::from(tunnel.socket_dir_or_default());
    if let Err(e) = std::fs::create_dir_all(&socket_dir) {
        error!(target: "server", dir = %socket_dir.display(), error = %e, "创建 docker_tunnel socket 目录失败");
        return;
    }

    // 0 表示不回收空闲连接
    let idle_timeout = (tunnel.idle_timeout_secs_or_default() > 0)
        .then(|| Duration::from_secs(tunnel.idle_timeout_secs_or_default()));
    let max_tunnels = tunnel.max_tunnels_or_default();

    let mut handles = Vec::new();
    let mut paths = Vec::new();

    // 并发名额按 Agent 建立：unix socket（D1-A）与 TCP（D1-B）两个入口共用同一份上限
    let semaphores: HashMap<Uuid, Arc<Semaphore>> = if max_tunnels > 0 {
        agents
            .iter()
            .map(|agent_uuid| (*agent_uuid, Arc::new(Semaphore::new(max_tunnels))))
            .collect()
    } else {
        HashMap::new()
    };

    for agent_uuid in &agents {
        let agent_uuid = *agent_uuid;
        let path = socket_dir.join(format!("{agent_uuid}.sock"));

        // 上次异常退出可能残留 socket 文件，bind 前必须先删除
        if path.exists()
            && let Err(e) = std::fs::remove_file(&path)
        {
            warn!(target: "server", path = %path.display(), error = %e, "清理残留 socket 文件失败");
        }

        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(e) => {
                error!(target: "server", path = %path.display(), error = %e, "绑定 docker_tunnel socket 失败");
                continue;
            }
        };

        if let Err(e) = prepare_socket_file(&path, tunnel) {
            warn!(target: "server", path = %path.display(), error = %e, "设置 socket 文件权限失败");
        }

        info!(target: "server", agent_uuid = %agent_uuid, path = %path.display(), "docker_tunnel 监听已启动");

        let runtime = AgentTunnelRuntime {
            agent_uuid,
            socket_path: path.clone(),
            idle_timeout,
            semaphore: semaphores.get(&agent_uuid).cloned(),
        };
        handles.push(tokio::spawn(accept_loop(listener, runtime)));
        paths.push(path);
    }

    // D1-B：可选的 TCP 入口（单端口 + 路径前缀），供跨机 Docker 客户端纳管
    if let Some(listen) = tunnel.tcp_listen() {
        let listen = listen.to_owned();
        let config = Arc::new(TcpEntryConfig {
            path_prefix: tunnel.path_prefix_or_default(),
            require_token: tunnel.require_token_or_default(),
            agents,
            semaphores,
        });
        handles.push(tokio::spawn(async move {
            match TcpListener::bind(&listen).await {
                Ok(listener) => {
                    info!(target: "server", listen = %listen, "docker_tunnel TCP 入口已启动");
                    tcp_accept_loop(listener, config).await;
                }
                Err(e) => {
                    error!(target: "server", listen = %listen, error = %e, "docker_tunnel TCP 入口绑定失败");
                }
            }
        }));
    }

    if handles.is_empty() {
        return;
    }

    RUNTIME_HANDLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(handles);
    SOCKET_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(paths);
}

/// 停止所有监听器，清理 socket 文件与待认领表。
pub fn stop() {
    let handles = std::mem::take(
        &mut *RUNTIME_HANDLES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for handle in handles {
        handle.abort();
    }

    PENDING_TUNNELS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();

    let paths = std::mem::take(
        &mut *SOCKET_PATHS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for path in paths {
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!(target: "server", path = %path.display(), error = %e, "删除 socket 文件失败");
        }
    }
}

/// 设置 socket 文件权限与属主/属组。
#[cfg(unix)]
fn prepare_socket_file(path: &Path, config: &DockerTunnelConfig) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(config.socket_file_mode_or_default()),
    )?;

    if config.socket_file_uid.is_some() || config.socket_file_gid.is_some() {
        std::os::unix::fs::chown(path, config.socket_file_uid, config.socket_file_gid)?;
    }

    Ok(())
}

/// 非 Unix 平台不支持 unix socket。
#[cfg(not(unix))]
fn prepare_socket_file(_path: &Path, _config: &DockerTunnelConfig) -> std::io::Result<()> {
    Ok(())
}

/// 接受本地管理端的连接，并为每条连接下发一次 `DockerTunnel` 任务。
async fn accept_loop(listener: UnixListener, runtime: AgentTunnelRuntime) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(value) => value,
            Err(e) => {
                warn!(target: "server", agent_uuid = %runtime.agent_uuid, error = %e, "docker_tunnel accept 失败");
                // accept 出错（如 FD 耗尽）时短暂退避，避免忙循环
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };

        // 并发名额：非阻塞获取，满则立即关闭这条本地连接（管理端会看到连接被拒）
        let permit = if let Some(semaphore) = runtime.semaphore.as_ref() {
            if let Ok(permit) = Arc::clone(semaphore).try_acquire_owned() {
                Some(permit)
            } else {
                warn!(
                    target: "server",
                    agent_uuid = %runtime.agent_uuid,
                    "docker_tunnel 并发隧道已达上限，拒绝本地连接"
                );
                continue;
            }
        } else {
            None
        };

        let agent_uuid = runtime.agent_uuid;
        let socket_path = runtime.socket_path.clone();
        let idle_timeout = runtime.idle_timeout;

        match claim_tunnel(agent_uuid, permit).await {
            Ok(claimed) => {
                tokio::spawn(pump_claimed_tunnel(
                    claimed,
                    stream,
                    agent_uuid,
                    socket_path,
                    idle_timeout,
                ));
            }
            Err(e) => {
                warn!(
                    target: "server",
                    agent_uuid = %agent_uuid,
                    error = %e,
                    "下发 docker_tunnel 任务失败，本地连接将被关闭"
                );
            }
        }
    }
}

/// 向 Agent 下发一次 `DockerTunnel` 任务并等待它拨入，返回已认领的隧道。
///
/// D1-A（unix socket）与 D1-B（TCP 入口）共用：两者都是「先登记待认领表、
/// 再下发任务」，区别只在拿到隧道之后怎么用——前者把隧道与本地 unix socket
/// 对拷，后者把隧道当作 hyper HTTP/1.1 的上游。
///
/// 注册顺序不能反：Agent 收到任务后可能立刻回连，若先下发再登记，
/// 会出现「Agent 已到、Server 还没登记」的竞态。
async fn claim_tunnel(
    agent_uuid: Uuid,
    permit: Option<OwnedSemaphorePermit>,
) -> anyhow::Result<ClaimedTunnel> {
    let (sender, receiver) = oneshot::channel();
    // 回调里才能拿到自增 task_id，超时/失败时要用它清理待认领表
    let registered: Arc<std::sync::OnceLock<u64>> = Arc::new(std::sync::OnceLock::new());
    let registered_in_callback = Arc::clone(&registered);

    let created = ng_task::create_task_internal(
        TaskManager::global(),
        agent_uuid,
        TaskEventType::DockerTunnel,
        move |task_id| {
            // Agent 侧收到的是无符号的任务 ID（`TaskEvent.task_id`），
            // 因此待认领表的键也要用无符号形式，否则两侧对不上。
            let task_id = task_id.cast_unsigned();
            let _ = registered_in_callback.set(task_id);
            PENDING_TUNNELS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((agent_uuid, task_id), PendingTunnel { sender, permit });
        },
    )
    .await;

    if let Err(e) = created {
        unregister_pending(agent_uuid, registered.get().copied());
        return Err(e);
    }

    match tokio::time::timeout(PENDING_CLAIM_TIMEOUT, receiver).await {
        Ok(Ok(claimed)) => Ok(claimed),
        Ok(Err(_)) => Err(anyhow::anyhow!("Agent 未认领隧道（待认领项已被清理）")),
        Err(_) => {
            unregister_pending(agent_uuid, registered.get().copied());
            Err(anyhow::anyhow!("等待 Agent 认领隧道超时"))
        }
    }
}

/// 从待认领表里移除一条登记（下发失败或等待超时时回收）。
fn unregister_pending(agent_uuid: Uuid, task_id: Option<u64>) {
    if let Some(task_id) = task_id {
        PENDING_TUNNELS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(agent_uuid, task_id));
    }
}

/// 与已认领的 Agent 隧道做裸字节对拷（D1-A）。
async fn pump_claimed_tunnel(
    claimed: ClaimedTunnel,
    stream: UnixStream,
    agent_uuid: Uuid,
    socket_path: PathBuf,
    idle_timeout: Option<Duration>,
) {
    debug!(
        target: "server",
        agent_uuid = %agent_uuid,
        path = %socket_path.display(),
        "docker_tunnel 已建立"
    );

    // 并发名额在认领后转移到这里，持有到隧道结束才释放。
    // 若把它留在待认领表里，认领的那一刻名额就被归还，`max_tunnels` 会提前失效。
    let _permit = claimed.permit;

    if let Err(e) = pump_bytes(stream, claimed.socket, idle_timeout).await {
        debug!(
            target: "server",
            agent_uuid = %agent_uuid,
            error = %e,
            "docker_tunnel 结束"
        );
    }
}

/// `/docker-tunnel` WebSocket 升级处理器。
async fn docker_tunnel_ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<TunnelParams>,
) -> impl IntoResponse {
    debug!(
        target: "server",
        agent_uuid = %params.agent_uuid,
        task_id = params.task_id,
        "收到 docker_tunnel 拨入请求"
    );
    // 与终端保持一致的上限：隧道里跑的是 Docker API，可能有较大的响应体
    ws.max_frame_size(1024 * 1024)
        .max_message_size(4 * 1024 * 1024)
        .on_upgrade(move |socket| handle_agent_socket(socket, params))
}

/// 处理 Agent 拨入的隧道连接：校验任务归属后交给等待中的本地连接。
///
/// 该 lint 在此是误报：待认领项里的并发名额不是多余资源，而是**故意**随 WebSocket
/// 一起转移给搬运任务（隧道真正结束才释放），因此不能在本函数内提前 drop。
#[allow(clippy::significant_drop_tightening)]
async fn handle_agent_socket(mut socket: WebSocket, params: TunnelParams) {
    let TunnelParams {
        agent_uuid,
        task_id,
        task_token,
    } = params;

    let agent_uuid = match check_tunnel_agent(&agent_uuid, &task_token, task_id).await {
        Ok(agent_uuid) => agent_uuid,
        Err(e) => {
            warn!(
                target: "server",
                agent_uuid = %agent_uuid,
                task_id = task_id,
                error = %e,
                "docker_tunnel 任务校验失败，拒绝拨入"
            );
            let nodeget_error = anyhow_to_nodeget_error(&e);
            let error_json =
                generate_error_message(nodeget_error.error_code(), &nodeget_error.to_string());
            let _ = socket
                .send(Message::Text(Utf8Bytes::from(error_json.to_string())))
                .await;
            return;
        }
    };

    // 取出待认领项；显式限定锁的作用域，保证 MutexGuard 在语句内释放、不跨 await
    let pending = {
        let mut table = PENDING_TUNNELS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        table.remove(&(agent_uuid, task_id))
    };

    let Some(PendingTunnel { sender, permit }) = pending else {
        warn!(
            target: "server",
            agent_uuid = %agent_uuid,
            task_id = task_id,
            "docker_tunnel 没有匹配的待认领连接，拒绝拨入"
        );
        let error_json = generate_error_message(102, "Permission Denied: No pending Docker tunnel");
        let _ = socket
            .send(Message::Text(Utf8Bytes::from(error_json.to_string())))
            .await;
        return;
    };

    if sender.send(ClaimedTunnel { socket, permit }).is_err() {
        debug!(
            target: "server",
            agent_uuid = %agent_uuid,
            task_id = task_id,
            "docker_tunnel 本地连接已消失，丢弃本次拨入"
        );
    }
}

/// 校验拨入方确实是某个 `docker_tunnel` 任务的 Agent。
///
/// 与终端一样按「任务 ID + Agent UUID + 任务令牌 + 结果未回写」四条件反查，
/// 并额外要求任务类型就是 `docker_tunnel`：否则这个端点会变成任意 socket 代理。
async fn check_tunnel_agent(
    agent_uuid: &str,
    task_token: &str,
    task_id: u64,
) -> anyhow::Result<Uuid> {
    let agent_uuid = Uuid::parse_str(agent_uuid)
        .map_err(|_| NodegetError::ParseError("Invalid Agent UUID format".to_owned()))?;

    let db = ng_db::get_db()
        .ok_or_else(|| NodegetError::DatabaseError("DB not initialized".to_owned()))?;

    let task_model = task::Entity::find()
        .filter(task::Column::Id.eq(task_id.cast_signed()))
        .filter(task::Column::Uuid.eq(agent_uuid))
        .filter(task::Column::Token.eq(task_token))
        .filter(task::Column::TaskEventResult.is_null())
        .one(db)
        .await
        .map_err(|e| NodegetError::DatabaseError(format!("Database error: {e}")))?;

    let Some(task_model) = task_model else {
        return Err(NodegetError::PermissionDenied(
            "Permission Denied: Invalid Task Token or ID".to_owned(),
        )
        .into());
    };

    let task_event_type: TaskEventType = serde_json::from_value(task_model.task_event_type)
        .map_err(|e| {
            NodegetError::SerializationError(format!("Failed to parse task_event_type: {e}"))
        })?;

    if !matches!(task_event_type, TaskEventType::DockerTunnel) {
        warn!(
            target: "server",
            agent_uuid = %agent_uuid,
            task_id = task_id,
            "权限拒绝: 非 docker_tunnel 任务尝试连接隧道端点"
        );
        return Err(NodegetError::PermissionDenied(
            "Docker tunnel connection is only allowed for docker_tunnel tasks".to_owned(),
        )
        .into());
    }

    Ok(agent_uuid)
}

/// 半关闭控制帧：某一方向不再有数据时发送，对端据此只关闭本地 socket 的写半边。
///
/// 数据一律走 `Binary`，`Text` 专用于控制帧，因此不会与 Docker 的报文混淆。
/// 这一点很关键：`docker run`/`attach`/`exec` 会先关闭本地连接的写半边（表示不再有
/// stdin），若把它当成整条连接结束，Docker daemon 后续写回的输出就会丢失。
const HALF_CLOSE_CONTROL: &str = "{\"t\":\"half_close\"}";

/// 在 unix socket 与 WebSocket 之间双向搬运字节。
///
/// 任一方向出错、对端关闭 WebSocket、或双向静默超过 `idle_timeout` 时返回。
/// 单个方向的 EOF 只做半关闭（通知对端 shutdown 写半边），不拆掉整条隧道。
async fn pump_bytes(
    stream: UnixStream,
    socket: WebSocket,
    idle_timeout: Option<Duration>,
) -> anyhow::Result<()> {
    let (mut sock_reader, mut sock_writer) = stream.into_split();
    let (mut ws_sink, mut ws_source) = socket.split();
    let mut buf = vec![0u8; TUNNEL_BUFFER_SIZE];

    // 未配置空闲回收时该分支永不就绪，收尾完全由两侧的读写分支负责
    let mut idle_deadline = idle_timeout.map(|timeout| tokio::time::Instant::now() + timeout);

    // 两个方向各自是否已收尾
    let mut local_done = false;
    let mut agent_done = false;

    loop {
        if local_done && agent_done {
            let _ = ws_sink.send(Message::Close(None)).await;
            return Ok(());
        }

        tokio::select! {
            biased;

            // 本地 Docker 客户端 → Agent
            read = sock_reader.read(&mut buf), if !local_done => {
                let n = read.map_err(|e| NodegetError::Other(format!("读取本地 socket 失败: {e}")))?;
                if n == 0 {
                    local_done = true;
                    ws_sink
                        .send(Message::Text(Utf8Bytes::from(HALF_CLOSE_CONTROL)))
                        .await
                        .map_err(|e| NodegetError::AgentConnectionError(format!("发送半关闭控制帧失败: {e}")))?;
                } else {
                    ws_sink
                        .send(Message::Binary(axum::body::Bytes::copy_from_slice(&buf[..n])))
                        .await
                        .map_err(|e| NodegetError::AgentConnectionError(format!("发送到 Agent 失败: {e}")))?;
                }
                idle_deadline = idle_timeout.map(|timeout| tokio::time::Instant::now() + timeout);
            }

            // Agent → 本地 Docker 客户端
            message = ws_source.next(), if !agent_done => {
                match message {
                    Some(Ok(Message::Binary(data))) => {
                        sock_writer
                            .write_all(&data)
                            .await
                            .map_err(|e| NodegetError::Other(format!("写入本地 socket 失败: {e}")))?;
                        idle_deadline = idle_timeout.map(|timeout| tokio::time::Instant::now() + timeout);
                    }
                    Some(Ok(Message::Text(text))) => {
                        if text.as_str() == HALF_CLOSE_CONTROL {
                            agent_done = true;
                            let _ = sock_writer.shutdown().await;
                        } else {
                            sock_writer
                                .write_all(text.as_bytes())
                                .await
                                .map_err(|e| NodegetError::Other(format!("写入本地 socket 失败: {e}")))?;
                            idle_deadline = idle_timeout.map(|timeout| tokio::time::Instant::now() + timeout);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        return Err(NodegetError::AgentConnectionError(format!("读取 Agent 消息失败: {e}")).into());
                    }
                }
            }

            // 空闲回收：双向静默超过阈值即关闭隧道
            () = sleep_until_deadline(idle_deadline) => {
                debug!(target: "server", "docker_tunnel 空闲超时，关闭隧道");
                return Ok(());
            }
        }
    }
}

/// 等待空闲回收时刻；未配置回收时永久等待。
async fn sleep_until_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

// ===========================================================================
// D1-B：TCP 入口（跨机纳管）
// ===========================================================================

/// 单条 TCP 连接上等待上游转发的请求队列长度
const UPSTREAM_REQUEST_QUEUE: usize = 8;

/// 单条请求等待上游响应头的最长时间（响应体是流式转发，不计入）
const UPSTREAM_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// TCP 入口的响应体：统一装箱，既能装上游（Docker daemon）的 `Incoming`，
/// 也能装本地构造的静态错误响应。
type TunnelBody = BoxBody<Bytes, hyper::Error>;

/// TCP 入口的连接级配置（一次 `start()` 内所有连接共享）。
struct TcpEntryConfig {
    /// 路径前缀（已归一化，空串表示 root）
    path_prefix: String,
    /// 是否要求 `Authorization: Bearer` 令牌
    require_token: bool,
    /// 允许的 Agent 白名单
    agents: Vec<Uuid>,
    /// 每个 Agent 的并发名额（`max_tunnels == 0` 时为空）
    semaphores: HashMap<Uuid, Arc<Semaphore>>,
}

/// 申请隧道名额的结果。
enum PermitOutcome {
    /// 成功（`None` 表示该 Agent 未配置并发上限）
    Granted(Option<OwnedSemaphorePermit>),
    /// 不在 `agents` 白名单内
    NotAllowed,
    /// 并发已达上限
    Exhausted,
}

impl TcpEntryConfig {
    /// 为该 Agent 申请一个隧道名额。
    fn try_acquire(&self, agent_uuid: Uuid) -> PermitOutcome {
        if !self.agents.contains(&agent_uuid) {
            return PermitOutcome::NotAllowed;
        }

        let Some(semaphore) = self.semaphores.get(&agent_uuid) else {
            return PermitOutcome::Granted(None);
        };

        Arc::clone(semaphore).try_acquire_owned().map_or_else(
            |_| PermitOutcome::Exhausted,
            |permit| PermitOutcome::Granted(Some(permit)),
        )
    }
}

/// 转发给上游 Agent 的一条 HTTP 请求。
struct UpstreamRequest {
    /// 已剥掉路径前缀的请求
    request: hyper::Request<hyper::body::Incoming>,
    /// 把上游响应（或失败原因）回给等待中的请求处理器
    respond:
        oneshot::Sender<std::result::Result<hyper::Response<hyper::body::Incoming>, UpstreamError>>,
}

/// 上游转发失败的原因，用于映射 HTTP 状态码。
enum UpstreamError {
    /// 等待上游响应头超时
    Timeout,
    /// 上游连接出错（隧道断开、Agent 侧关闭、报文不合法等）
    Transport(String),
}

/// 一条 TCP 连接上的上游隧道。
struct UpstreamTunnel {
    /// 把请求送进上游连接驱动任务
    sender: mpsc::Sender<UpstreamRequest>,
    /// 该连接的并发名额，持有到连接结束才释放
    _permit: Option<OwnedSemaphorePermit>,
}

/// TCP 入口的连接级状态（一个客户端连接对应一条隧道）。
#[derive(Default)]
struct TcpConnState {
    /// 已建立的上游隧道（懒建立：首个请求到来时才向 Agent 下发任务）
    upstream: Option<UpstreamTunnel>,
}

/// 把 Agent 拨入的 WebSocket 适配成字节流，供 hyper 的 HTTP/1.1 编解码器使用。
///
/// 数据（HTTP 报文）走 Binary 帧；Text 帧里只有 `half_close` 一种控制信令，
/// 语义是「对端不会再发数据」，对应本适配器的 EOF。`poll_shutdown` 也只发
/// `half_close` 而不关闭 WebSocket——另一半可能仍在传输 daemon 的响应。
struct TunnelIo {
    /// Agent 拨入的隧道
    socket: WebSocket,
    /// 已从 Binary 帧里取出但尚未交给 hyper 的字节
    read_buf: Vec<u8>,
    /// `read_buf` 中已交付的偏移
    read_pos: usize,
    /// 对端已声明不再发送数据
    read_eof: bool,
}

impl TunnelIo {
    /// 用一条已认领的隧道 WebSocket 构造。
    const fn new(socket: WebSocket) -> Self {
        Self {
            socket,
            read_buf: Vec::new(),
            read_pos: 0,
            read_eof: false,
        }
    }
}

impl AsyncRead for TunnelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.as_mut().get_mut();

        loop {
            if this.read_pos < this.read_buf.len() {
                let available = this.read_buf.len() - this.read_pos;
                let length = available.min(buf.remaining());
                buf.put_slice(&this.read_buf[this.read_pos..this.read_pos + length]);
                this.read_pos += length;
                if this.read_pos >= this.read_buf.len() {
                    this.read_buf.clear();
                    this.read_pos = 0;
                }
                return Poll::Ready(Ok(()));
            }

            if this.read_eof {
                // 不给任何字节即表示 EOF
                return Poll::Ready(Ok(()));
            }

            match this.socket.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None | Some(Ok(Message::Close(_)))) => {
                    this.read_eof = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(std::io::Error::other(e)));
                }
                Poll::Ready(Some(Ok(Message::Binary(data)))) => {
                    if !data.is_empty() {
                        this.read_buf.extend_from_slice(&data);
                    }
                }
                Poll::Ready(Some(Ok(Message::Text(text)))) => {
                    if text == HALF_CLOSE_CONTROL {
                        this.read_eof = true;
                        return Poll::Ready(Ok(()));
                    }
                }
                // Ping/Pong：由 WebSocket 层自动处理，不承载数据
                Poll::Ready(Some(Ok(_))) => {}
            }
        }
    }
}

impl AsyncWrite for TunnelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.as_mut().get_mut();

        match this.socket.poll_ready_unpin(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(std::io::Error::other(e))),
            Poll::Ready(Ok(())) => {}
        }

        // 数据必须走 Binary 帧：Text 帧被保留给 half_close 控制信令
        match this
            .socket
            .start_send_unpin(Message::Binary(Bytes::copy_from_slice(buf)))
        {
            Ok(()) => {
                // 让字节尽快离开发送队列，失败留给下一次 poll 暴露
                let _ = this.socket.poll_flush_unpin(cx);
                Poll::Ready(Ok(buf.len()))
            }
            Err(e) => Poll::Ready(Err(std::io::Error::other(e))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.as_mut().get_mut();
        this.socket
            .poll_flush_unpin(cx)
            .map_err(std::io::Error::other)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.as_mut().get_mut();

        match this.socket.poll_ready_unpin(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(std::io::Error::other(e))),
            Poll::Ready(Ok(())) => {}
        }

        // 只表达「本方向不再发数据」；WebSocket 本身由对端决定何时关闭
        if let Err(e) = this
            .socket
            .start_send_unpin(Message::Text(Utf8Bytes::from(HALF_CLOSE_CONTROL)))
        {
            return Poll::Ready(Err(std::io::Error::other(e)));
        }

        this.socket
            .poll_flush_unpin(cx)
            .map_err(std::io::Error::other)
    }
}

/// TCP 入口的 accept 循环。
async fn tcp_accept_loop(listener: TcpListener, config: Arc<TcpEntryConfig>) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(value) => value,
            Err(e) => {
                warn!(target: "server", error = %e, "docker_tunnel TCP accept 失败");
                // 与 unix 入口一致：accept 出错（如 FD 耗尽）时短暂退避，避免忙循环
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };

        let config = Arc::clone(&config);
        tokio::spawn(async move {
            serve_tcp_connection(stream, peer, config).await;
        });
    }
}

/// 用 hyper 的 HTTP/1.1 服务端跑一条 TCP 连接。
///
/// 用 hyper 而不是手写 framing：keep-alive、chunked、`Connection: close`、
/// 101 升级都由编解码层处理，本模块只在请求行/请求头上做前缀剥除与鉴权。
async fn serve_tcp_connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    config: Arc<TcpEntryConfig>,
) {
    let io = TokioIo::new(stream);
    let state = Arc::new(tokio::sync::Mutex::new(TcpConnState::default()));
    let service = hyper::service::service_fn(move |req| {
        let state = Arc::clone(&state);
        let config = Arc::clone(&config);
        async move { handle_tcp_request(req, state, config).await }
    });

    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, service)
        .with_upgrades()
        .await
    {
        debug!(target: "server", %peer, error = %e, "docker_tunnel TCP 连接结束");
    }
}

/// 处理一条 TCP 连接上的单个请求。
async fn handle_tcp_request(
    req: hyper::Request<hyper::body::Incoming>,
    state: Arc<tokio::sync::Mutex<TcpConnState>>,
    config: Arc<TcpEntryConfig>,
) -> std::result::Result<hyper::Response<TunnelBody>, Infallible> {
    match forward_tcp_request(req, &state, &config).await {
        Ok(response) => Ok(response),
        Err(status) => Ok(if status == hyper::StatusCode::UNAUTHORIZED {
            unauthorized_response()
        } else {
            error_response(status, status.canonical_reason().unwrap_or("error"))
        }),
    }
}

/// 剥掉路径前缀、鉴权，然后把请求交给（必要时新建的）上游隧道。
async fn forward_tcp_request(
    mut req: hyper::Request<hyper::body::Incoming>,
    state: &Arc<tokio::sync::Mutex<TcpConnState>>,
    config: &TcpEntryConfig,
) -> std::result::Result<hyper::Response<TunnelBody>, hyper::StatusCode> {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map_or("/", |value| value.as_str())
        .to_owned();

    let Some((agent_uuid, target)) = strip_path_prefix(&path_and_query, &config.path_prefix) else {
        debug!(target: "server", path = %path_and_query, "docker_tunnel TCP 请求路径不含合法前缀");
        return Err(hyper::StatusCode::NOT_FOUND);
    };

    // 客户端侧（Arcane/Docker CLI）的 101 升级句柄必须在把请求交给上游之前取出；
    // 没有升级意图时它只是一个会立刻失败的 future，无副作用。
    let client_upgrade = hyper::upgrade::on(&mut req);

    if config.require_token
        && check_request_token(req.headers(), agent_uuid)
            .await
            .is_err()
    {
        warn!(target: "server", agent_uuid = %agent_uuid, "docker_tunnel TCP 入口鉴权失败");
        return Err(hyper::StatusCode::UNAUTHORIZED);
    }

    // 令牌只用于 NodeGet 自身鉴权：转发给 Agent 前必须删除，
    // 免得与 D2-A 注入给远端 Docker daemon 的凭据混淆。
    req.headers_mut().remove(hyper::header::AUTHORIZATION);

    // 剥掉前缀（moby 的 basePath），query 原样保留
    let uri = target
        .parse::<hyper::Uri>()
        .map_err(|_| hyper::StatusCode::BAD_REQUEST)?;
    *req.uri_mut() = uri;

    let sender = ensure_upstream(state, config, agent_uuid).await?;

    let (respond_tx, respond_rx) = oneshot::channel();
    if sender
        .send(UpstreamRequest {
            request: req,
            respond: respond_tx,
        })
        .await
        .is_err()
    {
        return Err(hyper::StatusCode::BAD_GATEWAY);
    }

    let mut response = match respond_rx.await {
        Ok(Ok(response)) => response,
        Ok(Err(UpstreamError::Timeout)) => return Err(hyper::StatusCode::GATEWAY_TIMEOUT),
        Ok(Err(UpstreamError::Transport(_))) | Err(_) => {
            return Err(hyper::StatusCode::BAD_GATEWAY);
        }
    };

    // 101：两侧都要拿到「已升级」的裸字节流，而对拷必须等两侧握手都完成，
    // 因此取出上游句柄后立刻把 101 交回 hyper，由后台任务做字节对拷。
    if response.status() == hyper::StatusCode::SWITCHING_PROTOCOLS {
        let upstream_upgrade = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            match (client_upgrade.await, upstream_upgrade.await) {
                (Ok(client), Ok(upstream)) => {
                    let mut client = TokioIo::new(client);
                    let mut upstream = TokioIo::new(upstream);
                    if let Err(e) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await
                    {
                        debug!(target: "server", error = %e, "docker_tunnel hijack 字节搬运结束");
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    debug!(target: "server", error = %e, "docker_tunnel 升级失败");
                }
            }
        });
    }

    let (parts, body) = response.into_parts();
    Ok(hyper::Response::from_parts(parts, body.boxed()))
}

/// 确保当前连接有一条可用的上游隧道，返回其请求发送端。
async fn ensure_upstream(
    state: &Arc<tokio::sync::Mutex<TcpConnState>>,
    config: &TcpEntryConfig,
    agent_uuid: Uuid,
) -> std::result::Result<mpsc::Sender<UpstreamRequest>, hyper::StatusCode> {
    let mut guard = state.lock().await;
    if let Some(upstream) = guard.upstream.as_ref() {
        return Ok(upstream.sender.clone());
    }

    let permit = match config.try_acquire(agent_uuid) {
        PermitOutcome::Granted(permit) => permit,
        PermitOutcome::NotAllowed => {
            warn!(target: "server", agent_uuid = %agent_uuid, "docker_tunnel TCP 入口：Agent 不在白名单");
            return Err(hyper::StatusCode::FORBIDDEN);
        }
        PermitOutcome::Exhausted => {
            warn!(target: "server", agent_uuid = %agent_uuid, "docker_tunnel TCP 入口：并发隧道已达上限");
            return Err(hyper::StatusCode::SERVICE_UNAVAILABLE);
        }
    };

    // 下发任务并等待 Agent 拨入（最长 PENDING_CLAIM_TIMEOUT）
    let ClaimedTunnel { socket, permit } = claim_tunnel(agent_uuid, permit)
        .await
        .map_err(|e| {
            warn!(target: "server", agent_uuid = %agent_uuid, error = %e, "docker_tunnel TCP 入口：建立隧道失败");
            hyper::StatusCode::BAD_GATEWAY
        })?;

    let (sender, receiver) = mpsc::channel(UPSTREAM_REQUEST_QUEUE);
    tokio::spawn(run_upstream_connection(socket, receiver, agent_uuid));

    guard.upstream = Some(UpstreamTunnel {
        sender: sender.clone(),
        _permit: permit,
    });
    Ok(sender)
}

/// 在一条已认领的隧道上跑 HTTP/1.1 客户端，串行转发请求。
///
/// 客户端（比如 moby）用 keep-alive 复用一个连接，因此这里必须让 hyper 的
/// 连接在同一隧道上持续处理多个请求；隧道断开则本次连接的后续请求失败。
async fn run_upstream_connection(
    socket: WebSocket,
    mut receiver: mpsc::Receiver<UpstreamRequest>,
    agent_uuid: Uuid,
) {
    let io = TokioIo::new(TunnelIo::new(socket));
    let (mut sender, connection) = match hyper::client::conn::http1::handshake(io).await {
        Ok(value) => value,
        Err(e) => {
            warn!(target: "server", agent_uuid = %agent_uuid, error = %e, "docker_tunnel 上游 HTTP/1.1 握手失败");
            return;
        }
    };

    // 连接驱动任务：收发报文、处理 keep-alive，并在 101 时把裸字节流交出来
    let driver = tokio::spawn(async move {
        if let Err(e) = connection.with_upgrades().await {
            debug!(target: "server", agent_uuid = %agent_uuid, error = %e, "docker_tunnel 上游连接结束");
        }
    });

    while let Some(UpstreamRequest { request, respond }) = receiver.recv().await {
        let result =
            tokio::time::timeout(UPSTREAM_RESPONSE_TIMEOUT, sender.send_request(request)).await;
        let reply = match result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(UpstreamError::Transport(e.to_string())),
            Err(_) => Err(UpstreamError::Timeout),
        };
        let failed = reply.is_err();
        // 接收端可能已放弃（客户端断开），忽略发送失败
        let _ = respond.send(reply);

        if failed {
            break;
        }
    }

    driver.abort();
}

/// 校验 TCP 入口的 `Authorization: Bearer <token>`。
///
/// 语义上「处理这条 HTTP 请求」就是「为该 Agent 建立一条 `docker_tunnel` 隧道」，
/// 因此复用 `Task::Create("docker_tunnel")` 权限，而不是新增 Permission 变体：
/// 这样既有 token 的签发/前端权限项都不用改。
async fn check_request_token(headers: &hyper::HeaderMap, agent_uuid: Uuid) -> anyhow::Result<()> {
    let value = headers
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("缺少 Authorization 头"))?;

    let (scheme, token) = value
        .split_once(' ')
        .ok_or_else(|| anyhow::anyhow!("Authorization 头格式不正确"))?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(anyhow::anyhow!("只支持 Bearer 令牌"));
    }

    let token_or_auth =
        TokenOrAuth::from_full_token(token).map_err(|e| anyhow::anyhow!("令牌解析失败: {e}"))?;

    let checker = ng_core::permission::permission_checker::get_permission_checker()
        .ok_or_else(|| anyhow::anyhow!("PermissionChecker 未初始化"))?;

    let allowed = checker
        .check_token_limit(
            &token_or_auth,
            &[Scope::AgentUuid(agent_uuid)],
            &[Permission::Task(Task::Create(
                TaskEventType::DockerTunnel.task_name().to_owned(),
            ))],
        )
        .await?;

    if !allowed {
        return Err(anyhow::anyhow!("缺少该 Agent 的 docker_tunnel 权限"));
    }

    Ok(())
}

/// 从 `path_and_query` 里剥掉配置的路径前缀，返回 `(agent_uuid, 转发给 Agent 的路径)`。
///
/// 形如 `/agent/<uuid>` 或 `/agent/<uuid>/v1.47/containers/json?all=1`。
/// 前缀为空串时视为 root，即 `/<uuid>/...`。
fn strip_path_prefix(path_and_query: &str, prefix: &str) -> Option<(Uuid, String)> {
    let (path, query) = match path_and_query.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path_and_query, None),
    };

    // 前缀必须整段匹配且后接 `/`，避免 `/agentfoo/...` 被误判
    let rest = if prefix.is_empty() {
        path.strip_prefix('/')?
    } else {
        path.strip_prefix(prefix)?.strip_prefix('/')?
    };

    let (uuid_part, tail) = match rest.split_once('/') {
        Some((uuid_part, tail)) => (uuid_part, format!("/{tail}")),
        None => (rest, "/".to_owned()),
    };
    let agent_uuid = Uuid::parse_str(uuid_part).ok()?;

    let mut target = tail;
    if let Some(query) = query {
        target.push('?');
        target.push_str(query);
    }

    Some((agent_uuid, target))
}

/// 构造带文本体的错误响应。
fn error_response(status: hyper::StatusCode, message: &str) -> hyper::Response<TunnelBody> {
    let body = Full::new(Bytes::from(format!("{message}\n")))
        .map_err(|never| match never {})
        .boxed();
    let mut response = hyper::Response::new(body);
    *response.status_mut() = status;
    response
}

/// 401 响应：带上 `WWW-Authenticate` 提示客户端用 Bearer 令牌。
fn unauthorized_response() -> hyper::Response<TunnelBody> {
    let mut response = error_response(hyper::StatusCode::UNAUTHORIZED, "unauthorized");
    response.headers_mut().insert(
        hyper::header::WWW_AUTHENTICATE,
        hyper::header::HeaderValue::from_static("Bearer realm=\"nodeget\""),
    );
    response
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// 插入一条任务记录，返回任务 ID。
    async fn insert_task(
        db: &sea_orm::DatabaseConnection,
        agent_uuid: Uuid,
        token: &str,
        task_event_type: serde_json::Value,
    ) -> i64 {
        use sea_orm::{ActiveValue, Set};

        task::Entity::insert(task::ActiveModel {
            id: ActiveValue::default(),
            uuid: Set(agent_uuid),
            token: Set(token.to_owned()),
            cron_source: Set(None),
            timestamp: Set(None),
            success: Set(None),
            error_message: Set(None),
            task_event_type: Set(task_event_type),
            task_event_result: Set(None),
            created_at: Set(None),
        })
        .exec(db)
        .await
        .unwrap()
        .last_insert_id
    }

    /// 拨入鉴权是安全边界：任务三元组匹配还不够，任务类型必须是 `docker_tunnel`。
    ///
    /// 否则任何持有其他任务（例如 `WebShell`）`task_token` 的调用方都能用这个端点
    /// 把任意 socket 代理出去。
    #[tokio::test]
    async fn check_tunnel_agent_rejects_other_task_types() {
        // 内存库必须限制为单连接：连接池 > 1 时每个连接是各自独立的内存数据库
        let config = ng_db::DbConnectionConfig {
            database_url: "sqlite::memory:".to_owned(),
            max_connections: 1,
            ..ng_db::DbConnectionConfig::default()
        };
        ng_db::init_db_connection(config).await.unwrap();
        let db = ng_db::get_db().unwrap();

        let agent_uuid = Uuid::new_v4();

        // 1) WebShell 任务的 task_token：即使 id/uuid/token 完全匹配也必须拒绝
        let webshell_id = insert_task(
            db,
            agent_uuid,
            "ws-token",
            serde_json::json!({
                "web_shell": {
                    "url": "ws://127.0.0.1:1/auto_gen",
                    "terminal_id": Uuid::new_v4(),
                }
            }),
        )
        .await;
        let err = check_tunnel_agent(
            &agent_uuid.to_string(),
            "ws-token",
            webshell_id.cast_unsigned(),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("only allowed for docker_tunnel"),
            "unexpected error: {err}"
        );

        // 2) docker_tunnel 任务：通过
        let tunnel_id = insert_task(
            db,
            agent_uuid,
            "dt-token",
            serde_json::json!("docker_tunnel"),
        )
        .await;
        assert!(
            check_tunnel_agent(
                &agent_uuid.to_string(),
                "dt-token",
                tunnel_id.cast_unsigned()
            )
            .await
            .is_ok()
        );

        // 3) 令牌不匹配：拒绝
        let err = check_tunnel_agent(
            &agent_uuid.to_string(),
            "wrong-token",
            tunnel_id.cast_unsigned(),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("Invalid Task Token"),
            "unexpected error: {err}"
        );

        // 4) Agent UUID 不匹配：拒绝
        let other_uuid = Uuid::new_v4();
        assert!(
            check_tunnel_agent(
                &other_uuid.to_string(),
                "dt-token",
                tunnel_id.cast_unsigned()
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn router_registers_docker_tunnel_route() {
        // 路由能构建即可（实际路由断言在 axum 内部，这里只保证不 panic）
        let _ = router();
    }

    #[test]
    fn prepare_socket_file_sets_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("nodeget-docker-tunnel-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sock");
        std::fs::write(&path, b"").unwrap();

        let config = DockerTunnelConfig {
            enable: Some(true),
            socket_dir: None,
            tcp_listen: None,
            path_prefix: None,
            require_token: None,
            agents: None,
            socket_file_mode: Some("0640".to_owned()),
            socket_file_uid: None,
            socket_file_gid: None,
            idle_timeout_secs: None,
            max_tunnels: None,
        };
        prepare_socket_file(&path, &config).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 路径前缀剥除：moby 的 basePath 会出现在每一个请求上，且 query 必须原样保留。
    #[test]
    fn strip_path_prefix_extracts_uuid_and_keeps_query() {
        let uuid = Uuid::new_v4();

        let path = format!("/agent/{uuid}/v1.47/containers/json?all=1");
        let (parsed, target) = strip_path_prefix(&path, "/agent").unwrap();
        assert_eq!(parsed, uuid);
        assert_eq!(target, "/v1.47/containers/json?all=1");

        // 版本协商走 `/_ping`
        assert_eq!(
            strip_path_prefix(&format!("/agent/{uuid}/_ping"), "/agent")
                .unwrap()
                .1,
            "/_ping"
        );

        // 只有前缀 + uuid、没有后续路径
        assert_eq!(
            strip_path_prefix(&format!("/agent/{uuid}"), "/agent")
                .unwrap()
                .1,
            "/"
        );

        // 空前缀等价于 root
        assert_eq!(
            strip_path_prefix(&format!("/{uuid}/_ping"), "").unwrap().1,
            "/_ping"
        );
    }

    /// 路径前缀剥除的负例：前缀不整段匹配、uuid 非法、前缀后不是 `/`。
    #[test]
    fn strip_path_prefix_rejects_invalid_paths() {
        let uuid = Uuid::new_v4();

        assert!(strip_path_prefix(&format!("/agentfoo/{uuid}/_ping"), "/agent").is_none());
        assert!(strip_path_prefix(&format!("/agents/{uuid}/_ping"), "/agent").is_none());
        assert!(strip_path_prefix("/agent/not-a-uuid/_ping", "/agent").is_none());
        assert!(strip_path_prefix("/_ping", "/agent").is_none());
    }
}
