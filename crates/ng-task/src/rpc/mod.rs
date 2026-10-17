//! Task RPC 命名空间：提供 JSON-RPC 方法供客户端调度和管理任务。
//!
//! 命名空间前缀为 `task`，包含以下方法：
//! - `task_register_task` — 订阅任务事件流（WebSocket）
//! - `task_create_task` — 创建任务并立即返回
//! - `task_create_task_blocking` — 创建任务并阻塞等待 Agent 返回结果
//! - `task_upload_task_result` — Agent 上传任务执行结果
//! - `task_query` — 查询任务记录
//! - `task_delete` — 删除任务记录
//!
//! 权限校验委托至全局 `ng_core::permission::permission_checker::PermissionChecker`。
//! `MonitoringUuidProvider` trait 仍由服务器二进制注入。

use jsonrpsee::PendingSubscriptionSink;
use jsonrpsee::SubscriptionMessage;
use jsonrpsee::core::{JsonRawValue, RpcResult, SubscriptionResult};
use jsonrpsee::proc_macros::rpc;
use ng_core::error::NodegetError;
use ng_core::permission::data_structure::{Permission, Scope, Task};
use ng_core::permission::token_auth::TokenOrAuth;
use ng_core::utils::JsonError;
use ng_db::rpc::{RpcHelper, token_identity};
use ng_db::rpc_exec;
use serde_json::value::RawValue;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::{Instrument, debug, trace};
use uuid::Uuid;

/// 转义 SQL LIKE 特殊字符，防止注入攻击。
///
/// SQL LIKE 中 `%` 匹配任意字符序列，`_` 匹配单个字符，这些字符需要转义才能在
/// JSON 文本搜索中进行精确匹配。配合 `.escape('\\')` 使用。
pub(crate) fn escape_like_pattern(pattern: &str) -> String {
    pattern
        .replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_")
}

mod create_task;
mod create_task_blocking;
mod delete;
mod query;
mod upload_task_result;

// 供服务器二进制内部派发复用（无权限校验，调用方自负鉴权责任），见 create_task 模块文档
pub use create_task::create_task_internal;

// ── Monitoring UUID provider trait ──────────────────────────────

/// 监控 UUID 缓存操作 trait，由服务器二进制实现并注入
///
/// 任务创建时需确保目标 Agent UUID 已注册到 monitoring_uuid 表
pub trait MonitoringUuidProvider: Send + Sync + 'static {
    /// 获取或插入 UUID 到 monitoring_uuid 表，返回对应 i16 ID
    fn get_or_insert(
        &self,
        uuid: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i16, NodegetError>> + Send>>;

    /// 刷新 monitoring_uuid 缓存
    fn reload(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
}

/// 全局 MonitoringUuidProvider 单例，由服务器启动时注入
static MONITORING_UUID_PROVIDER: OnceLock<Arc<dyn MonitoringUuidProvider>> = OnceLock::new();

/// 设置全局 MonitoringUuidProvider，服务器启动时调用一次
pub fn set_monitoring_uuid_provider(provider: Arc<dyn MonitoringUuidProvider>) {
    let _ = MONITORING_UUID_PROVIDER.set(provider);
}

/// 获取全局 MonitoringUuidProvider，未初始化时返回 None
pub fn monitoring_uuid_provider() -> Option<&'static Arc<dyn MonitoringUuidProvider>> {
    MONITORING_UUID_PROVIDER.get()
}

// ── RPC trait 定义 ──────────────────────────────────────────

/// Task RPC 接口定义，所有方法返回 `RpcResult<Box<RawValue>>` 以统一日志格式
#[rpc(server, namespace = "task")]
pub trait Rpc {
    /// 订阅任务事件流，Agent 通过此方法接收待执行的任务
    #[subscription(name = "register_task", item = crate::types::TaskEvent, unsubscribe = "unregister_task"
    )]
    async fn register_task(&self, token: String, uuid: Uuid) -> SubscriptionResult;

    /// 创建任务并立即返回任务 ID
    #[method(name = "create_task")]
    async fn create_task(
        &self,
        token: String,
        target_uuid: Uuid,
        task_type: crate::types::TaskEventType,
    ) -> RpcResult<Box<RawValue>>;

    /// 创建任务并阻塞等待 Agent 返回结果，超时则返回错误
    #[method(name = "create_task_blocking")]
    async fn create_task_blocking(
        &self,
        token: String,
        target_uuid: Uuid,
        task_type: crate::types::TaskEventType,
        timeout_ms: u64,
    ) -> RpcResult<Box<RawValue>>;

    /// Agent 上传任务执行结果
    #[method(name = "upload_task_result")]
    async fn upload_task_result(
        &self,
        token: String,
        task_response: crate::types::TaskEventResponse,
    ) -> RpcResult<Box<RawValue>>;

    /// 查询任务记录，支持多条件过滤
    #[method(name = "query")]
    async fn query(
        &self,
        token: String,
        task_data_query: crate::types::query::TaskDataQuery,
    ) -> RpcResult<Box<RawValue>>;

    /// 删除任务记录，支持多条件过滤
    #[method(name = "delete")]
    async fn delete(
        &self,
        token: String,
        conditions: Vec<crate::types::query::TaskQueryCondition>,
    ) -> RpcResult<Box<RawValue>>;
}

/// Task RPC 实现，持有 `TaskManager` 和 `RpcHelper` 提供的数据库访问能力
pub struct TaskRpcImpl {
    /// 任务广播管理器，负责会话注册、事件分发和 blocking waiter 管理
    pub manager: Arc<TaskManager>,
}

impl RpcHelper for TaskRpcImpl {}

use jsonrpsee::core::async_trait;

#[async_trait]
impl RpcServer for TaskRpcImpl {
    async fn create_task(
        &self,
        token: String,
        target_uuid: Uuid,
        task_type: crate::types::TaskEventType,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "task", "task::create_task", token_key = tk, username = un, target_uuid = %target_uuid, task_type = ?task_type);
        async {
            rpc_exec!(create_task::create_task(&self.manager, token, target_uuid, task_type).await)
        }
        .instrument(span)
        .await
    }

    async fn create_task_blocking(
        &self,
        token: String,
        target_uuid: Uuid,
        task_type: crate::types::TaskEventType,
        timeout_ms: u64,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "task", "task::create_task_blocking", token_key = tk, username = un, target_uuid = %target_uuid, task_type = ?task_type, timeout_ms = timeout_ms);
        async {
            rpc_exec!(
                create_task_blocking::create_task_blocking(
                    &self.manager,
                    token,
                    target_uuid,
                    task_type,
                    timeout_ms,
                )
                .await
            )
        }
        .instrument(span)
        .await
    }

    async fn upload_task_result(
        &self,
        token: String,
        task_response: crate::types::TaskEventResponse,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "task", "task::upload_task_result", token_key = tk, username = un, task_id = %task_response.task_id, agent_uuid = %task_response.agent_uuid);
        async {
            rpc_exec!(
                upload_task_result::upload_task_result(&self.manager, token, task_response).await
            )
        }
        .instrument(span)
        .await
    }

    async fn query(
        &self,
        token: String,
        task_data_query: crate::types::query::TaskDataQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "task", "task::query", token_key = tk, username = un, query = ?task_data_query);
        async { rpc_exec!(query::query(token, task_data_query).await) }
            .instrument(span)
            .await
    }

    async fn delete(
        &self,
        token: String,
        conditions: Vec<crate::types::query::TaskQueryCondition>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "task", "task::delete", token_key = tk, username = un, conditions = ?conditions);
        async { rpc_exec!(delete::delete(token, conditions).await) }
            .instrument(span)
            .await
    }

    async fn register_task(
        &self,
        subscription_sink: PendingSubscriptionSink,
        token: String,
        uuid: Uuid,
    ) -> SubscriptionResult {
        // Task 订阅注册：鉴权 → 权限校验 → 建立会话 → 启动转发协程
        // 1. 解析 TokenOrAuth 格式，失败则 reject 订阅
        // 2. 获取 AuthProvider，校验 Task::Listen 权限（scope 为目标 Agent UUID）
        // 3. 权限通过后 accept sink，向 TaskManager 注册 (uuid, reg_id, tx) 会话
        // 4. spawn 协程从 mpsc 读取 TaskEvent → 序列化为 RawValue → 推送到 WebSocket sink
        // 5. 断连或序列化失败时从 TaskManager 移除会话
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "task", "task::register_task", token_key = tk, username = un, uuid = %uuid);
        let _guard = span.enter();

        tracing::info!(target: "task", "subscription requested");

        // 解析 Token 格式，失败则拒绝订阅
        let Ok(token_or_auth) = TokenOrAuth::from_full_token(&token) else {
            tracing::error!(target: "task", "token parse error, rejecting subscription");
            subscription_sink
                .reject(jsonrpsee::types::ErrorObject::borrowed(
                    101,
                    "Token Parse Error",
                    None,
                ))
                .await;
            return Ok(());
        };

        // 获取 PermissionChecker，未初始化则拒绝
        let provider = ng_core::permission::permission_checker::get_permission_checker()
            .ok_or_else(|| {
                jsonrpsee::types::ErrorObject::borrowed(
                    101,
                    "PermissionChecker not initialized",
                    None,
                )
            })
            .ok();

        let Some(provider) = provider else {
            subscription_sink
                .reject(jsonrpsee::types::ErrorObject::borrowed(
                    101,
                    "PermissionChecker not initialized",
                    None,
                ))
                .await;
            return Ok(());
        };

        // 检查 Task::Listen 权限，scope 为目标 Agent UUID
        let is_allowed_result = provider
            .check_token_limit(
                &token_or_auth,
                &[Scope::AgentUuid(uuid)],
                &[Permission::Task(Task::Listen)],
            )
            .await;

        match is_allowed_result {
            Ok(true) => {
                tracing::debug!(target: "task", "register_task permission check passed");
            }
            Ok(false) => {
                tracing::error!(target: "task", "permission denied, rejecting subscription");
                subscription_sink
                    .reject(jsonrpsee::types::ErrorObject::borrowed(
                        102,
                        "Permission Denied: Missing Task Listen permission for this Agent",
                        None,
                    ))
                    .await;
                return Ok(());
            }
            Err(e) => {
                let nodeget_err = ng_core::error::anyhow_to_nodeget_error(&e);
                tracing::error!(target: "task", error = %nodeget_err, "permission check failed, rejecting subscription");
                let () = subscription_sink
                    .reject(jsonrpsee::types::ErrorObject::owned(
                        nodeget_err.error_code() as i32,
                        nodeget_err.to_string(),
                        None::<JsonError>,
                    ))
                    .await;
                return Ok(());
            }
        }

        // 权限校验通过，接受订阅并注册会话
        let sink = subscription_sink.accept().await?;
        let (tx, mut rx) = mpsc::channel(32);
        let reg_id = Uuid::new_v4();

        self.manager.add_session(uuid, reg_id, tx).await;
        tracing::info!(target: "task", reg_id = %reg_id, "subscription accepted");

        let manager_clone = self.manager.clone();
        let uuid_clone = uuid;
        let reg_id_clone = reg_id;

        // 在 spawn 之前释放 span guard，避免 spawned task 继承当前 span
        drop(_guard);
        let forward_span = span.clone();

        // 消息转发协程：将 mpsc 接收到的 TaskEvent 序列化后推送到 WebSocket
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let json_str = match serde_json::to_string(&msg) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(target: "task", error = %e, "failed to serialize task event");
                        break;
                    }
                };

                let Ok(raw_value) = JsonRawValue::from_string(json_str) else {
                    tracing::error!(target: "task", "failed to create JsonRawValue");
                    break;
                };

                let sub_msg = SubscriptionMessage::from(raw_value);

                if sink.send(sub_msg).await.is_err() {
                    break;
                }
            }

            // 客户端断连或序列化失败，清理会话
            manager_clone
                .remove_session(&uuid_clone, &reg_id_clone)
                .await;
            tracing::info!(target: "task", uuid = %uuid_clone, reg_id = %reg_id_clone, "client disconnected, session removed");
        }.instrument(forward_span));

        Ok(())
    }
}

// ── TaskManager ────────────────────────────────────────────────

/// 已连接 Agent 的会话表：UUID → (注册 ID, 任务事件发送端)
type Peers = Arc<RwLock<HashMap<Uuid, (Uuid, mpsc::Sender<crate::types::TaskEvent>)>>>;

/// Blocking waiter 表：task_id → oneshot 发送端，用于 `create_task_blocking` 等待结果
/// 临界区无 .await，使用 std::sync::RwLock 避免 tokio async 开销
type BlockingWaiters =
    Arc<std::sync::RwLock<HashMap<u64, oneshot::Sender<crate::types::TaskEventResponse>>>>;

/// 全局 TaskManager 单例，延迟初始化
static GLOBAL_TASK_MANAGER: OnceLock<Arc<TaskManager>> = OnceLock::new();

/// 任务事件发送失败的原因。
///
/// 调用方据此区分两种语义完全不同的情况：
/// - [`TaskSendError::NotConnected`]：Agent 离线。时间敏感任务（ping / IP 查询等）
///   跳过即可，不应记录为失败；RPC 路径应返回 `NodegetError::AgentOffline`。
/// - [`TaskSendError::QueueFull`]：Agent 在线但任务队列已满，属可重试的繁忙状态，
///   应记为失败并保留告警。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSendError {
    /// 目标 Agent 无活动任务会话（离线，或发送瞬间断连）
    NotConnected(Uuid),
    /// 目标 Agent 任务队列（容量 32）已满
    QueueFull(Uuid),
}

impl TaskSendError {
    /// 目标 Agent UUID
    #[must_use]
    pub const fn uuid(&self) -> Uuid {
        match self {
            Self::NotConnected(uuid) | Self::QueueFull(uuid) => *uuid,
        }
    }

    /// 是否为「离线」类失败（可跳过）
    #[must_use]
    pub const fn is_not_connected(&self) -> bool {
        matches!(self, Self::NotConnected(_))
    }

    /// 人类可读消息。文案与历史版本保持一致，避免客户端按文本匹配失效。
    #[must_use]
    pub fn message(&self) -> String {
        let uuid = self.uuid();
        match self {
            Self::NotConnected(_) => format!("Agent {uuid} is not connected"),
            Self::QueueFull(_) => format!("Agent {uuid} task queue is full"),
        }
    }
}

/// 任务广播管理器，负责 Agent 会话注册、任务事件分发和 blocking waiter 管理
///
/// 每个 Agent 通过 WebSocket 订阅 `register_task`，在此注册一个 mpsc Sender；
/// 创建任务时通过 `send_event` 将 `TaskEvent` 发送到对应 Sender；
/// `create_task_blocking` 通过 oneshot channel 等待 Agent 返回结果。
#[derive(Clone)]
pub struct TaskManager {
    /// 已连接 Agent 的会话表
    peers: Peers,
    /// 等待结果的 blocking waiter 表
    blocking_waiters: BlockingWaiters,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    /// 创建新的 TaskManager 实例
    #[must_use]
    pub fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
            blocking_waiters: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// 获取全局 TaskManager 单例，首次调用时初始化
    #[must_use]
    pub fn global() -> &'static Arc<Self> {
        GLOBAL_TASK_MANAGER.get_or_init(|| Arc::new(Self::new()))
    }

    /// 注册 Agent 会话，UUID 相同时覆盖旧会话
    ///
    /// - `uuid` — Agent 的 UUID
    /// - `reg_id` — 本次订阅的注册 ID，用于后续精确移除
    /// - `tx` — 任务事件发送端，Agent 消费端接收 `TaskEvent`
    pub async fn add_session(
        &self,
        uuid: Uuid,
        reg_id: Uuid,
        tx: mpsc::Sender<crate::types::TaskEvent>,
    ) {
        self.peers.write().await.insert(uuid, (reg_id, tx));
        debug!(target: "task", uuid = %uuid, reg_id = %reg_id, "session registered");
    }

    /// 移除 Agent 会话，仅当 reg_id 匹配时才移除（防止误删新会话）
    pub async fn remove_session(&self, uuid: &Uuid, reg_id: &Uuid) {
        let mut peers = self.peers.write().await;

        if let Some((current_reg_id, _)) = peers.get(uuid)
            && current_reg_id == reg_id
        {
            peers.remove(uuid);
            debug!(target: "task", uuid = %uuid, reg_id = %reg_id, "session removed");
        }
        drop(peers);
    }

    /// 向指定 Agent 发送任务事件
    ///
    /// - `uuid` — 目标 Agent UUID
    /// - `event` — 任务事件
    ///
    /// 失败原因由 [`TaskSendError`] 区分：`NotConnected`（离线，可跳过）与
    /// `QueueFull`（在线但忙，应重试）。此前两者共用错误码 104，调用方无法区分。
    ///
    /// 本函数不写日志：级别由调用方按场景决定（crontab 对离线记 debug、对队列满
    /// 记 warn；RPC 路径对离线返回 `AgentOffline`）。
    pub async fn send_event(
        &self,
        uuid: Uuid,
        event: crate::types::TaskEvent,
    ) -> Result<(), TaskSendError> {
        trace!(target: "task", uuid = %uuid, "sending task event");
        // 锁内仅 clone 廉价的 Sender，drop 读锁后再 await send，
        // 避免持锁期间 await send 导致 add_session/remove_session 的写锁被队头阻塞。
        let tx = {
            let peers = self.peers.read().await;
            match peers.get(&uuid) {
                Some((_, tx)) => tx.clone(),
                None => return Err(TaskSendError::NotConnected(uuid)),
            }
        };

        // 用 try_send 而非 send().await：agent 任务队列（容量 32）满时立即返回错误，
        // 而非阻塞挂起 RPC handler 等待排空（慢 agent 场景下会拖挂 create_task）。
        // 调用方可据此向客户端返回 503/重试提示。
        match tx.try_send(event) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                Err(TaskSendError::QueueFull(uuid))
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(TaskSendError::NotConnected(uuid))
            }
        }
    }

    /// 判断指定 Agent 当前是否有活动任务会话（在线）。
    ///
    /// 用于派发前预检，避免对离线目标走「写库 → 发送失败 → 回滚」的路径。
    /// 语义是「有活动任务订阅会话」，不是「机器存活」：进程僵死或 TCP 半开时仍返回
    /// `true`，那种情况只能靠 `send_event` 的失败兜底。
    pub async fn is_connected(&self, uuid: &Uuid) -> bool {
        self.peers.read().await.contains_key(uuid)
    }

    /// 过滤出当前已连接的 Agent UUID，保持传入顺序。
    ///
    /// 用于派发前预筛：对未连接的 Agent 下发任务必然返回 104，除了一次无意义的
    /// INSERT/UPDATE/DELETE，还会写一条失败的 `crontab_result` 并刷 WARN 日志
    /// （生产实测：单个离线 Agent 在 6 条 20 秒周期的 cron 下每天产生约 2.6 万行无效记录）。
    pub async fn connected_uuids(&self, candidates: &[Uuid]) -> Vec<Uuid> {
        // 单次读锁内完成全部判断，避免逐 Agent 取锁。
        let peers = self.peers.read().await;
        candidates
            .iter()
            .copied()
            .filter(|uuid| peers.contains_key(uuid))
            .collect()
    }

    /// 注册一个 blocking waiter，等待指定 `task_id` 的结果
    pub fn register_blocking_waiter(
        &self,
        task_id: u64,
    ) -> oneshot::Receiver<crate::types::TaskEventResponse> {
        let (tx, rx) = oneshot::channel();
        self.blocking_waiters
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task_id, tx);
        debug!(target: "task", task_id = task_id, "blocking waiter registered");
        rx
    }

    /// 移除 blocking waiter（超时或取消时调用）
    pub fn remove_blocking_waiter(&self, task_id: u64) {
        self.blocking_waiters
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&task_id);
    }

    /// 尝试通知 blocking waiter（upload_task_result 时调用）
    ///
    /// 返回 `true` 表示有 waiter 被通知，`false` 表示无人在等待
    pub fn notify_blocking_waiter(
        &self,
        task_id: u64,
        response: crate::types::TaskEventResponse,
    ) -> bool {
        let value = self
            .blocking_waiters
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&task_id);
        value.is_some_and(|tx| {
            let _ = tx.send(response);
            debug!(target: "task", task_id = task_id, "blocking waiter notified");
            true
        })
    }
}

/// 构建 Task RPC 模块，注册所有 `task_*` 方法，用于合并到服务器 RPC 路由
pub fn rpc_module() -> jsonrpsee::RpcModule<TaskRpcImpl> {
    TaskRpcImpl {
        manager: TaskManager::global().clone(),
    }
    .into_rpc()
}

#[cfg(test)]
mod tests {
    use super::{TaskManager, TaskSendError};
    use crate::types::{TaskEvent, TaskEventType};
    use uuid::Uuid;

    fn event(task_id: u64) -> TaskEvent {
        TaskEvent {
            task_id,
            task_token: "tok".to_owned(),
            task_event_type: TaskEventType::Ip,
        }
    }

    #[tokio::test]
    async fn is_connected_reflects_session_lifecycle() {
        let manager = TaskManager::new();
        let uuid = Uuid::new_v4();
        let reg_id = Uuid::new_v4();

        assert!(!manager.is_connected(&uuid).await, "no session yet");

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        manager.add_session(uuid, reg_id, tx).await;
        assert!(manager.is_connected(&uuid).await, "session registered");

        // reg_id 不匹配时不应误删新会话
        manager.remove_session(&uuid, &Uuid::new_v4()).await;
        assert!(
            manager.is_connected(&uuid).await,
            "mismatched reg_id keeps session"
        );

        manager.remove_session(&uuid, &reg_id).await;
        assert!(!manager.is_connected(&uuid).await, "session removed");
    }

    #[tokio::test]
    async fn send_event_reports_not_connected_without_session() {
        let manager = TaskManager::new();
        let uuid = Uuid::new_v4();

        let err = manager
            .send_event(uuid, event(1))
            .await
            .expect_err("send without session must fail");

        assert!(err.is_not_connected());
        assert_eq!(err.uuid(), uuid);
        assert_eq!(err.message(), format!("Agent {uuid} is not connected"));
    }

    #[tokio::test]
    async fn send_event_delivers_to_registered_session() {
        let manager = TaskManager::new();
        let uuid = Uuid::new_v4();
        let reg_id = Uuid::new_v4();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        manager.add_session(uuid, reg_id, tx).await;
        manager.send_event(uuid, event(42)).await.expect("send");

        let received = rx.recv().await.expect("event delivered");
        assert_eq!(received.task_id, 42);
    }

    #[tokio::test]
    async fn send_event_reports_queue_full_when_agent_is_busy() {
        let manager = TaskManager::new();
        let uuid = Uuid::new_v4();
        let reg_id = Uuid::new_v4();
        // 容量 1：塞满后再发即触发 QueueFull（与真实 agent 队列容量 32 的行为一致）
        let (tx, _rx) = tokio::sync::mpsc::channel(1);

        manager.add_session(uuid, reg_id, tx).await;
        manager
            .send_event(uuid, event(1))
            .await
            .expect("first send");
        let err = manager
            .send_event(uuid, event(2))
            .await
            .expect_err("second send must fail");

        assert!(
            !err.is_not_connected(),
            "queue full is not an offline condition"
        );
        assert_eq!(err.uuid(), uuid);
        assert_eq!(err.message(), format!("Agent {uuid} task queue is full"));
    }

    #[tokio::test]
    async fn send_event_reports_not_connected_when_receiver_dropped() {
        let manager = TaskManager::new();
        let uuid = Uuid::new_v4();
        let reg_id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::mpsc::channel(4);

        manager.add_session(uuid, reg_id, tx).await;
        drop(rx); // agent 断开但会话尚未清理

        let err = manager
            .send_event(uuid, event(1))
            .await
            .expect_err("closed channel must fail");
        assert!(err.is_not_connected());
        assert_eq!(err.message(), format!("Agent {uuid} is not connected"));
    }

    #[test]
    fn connected_uuids_preserves_order_and_filters_offline() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let manager = TaskManager::new();
            let online = Uuid::new_v4();
            let offline = Uuid::new_v4();
            let (tx, _rx) = tokio::sync::mpsc::channel(4);
            manager.add_session(online, Uuid::new_v4(), tx).await;

            let filtered = manager.connected_uuids(&[offline, online]).await;
            assert_eq!(filtered, vec![online]);
        });
    }

    #[test]
    fn task_send_error_message_keeps_legacy_wording() {
        let uuid = Uuid::new_v4();
        assert_eq!(
            TaskSendError::NotConnected(uuid).message(),
            format!("Agent {uuid} is not connected")
        );
        assert_eq!(
            TaskSendError::QueueFull(uuid).message(),
            format!("Agent {uuid} task queue is full")
        );
    }
}
