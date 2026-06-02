//! 权限校验 —— JS Worker 和 JS Result 的 RBAC 权限检查。
//!
//! 提供 `TokenPermissionChecker` trait（依赖注入点）和一系列权限校验辅助函数：
//! - `check_js_worker_permission` —— 检查 Worker 级别权限
//! - `check_get_rt_pool_permission` —— 检查运行时池查看权限
//! - `filter_workers_by_list_permission` —— 按列表权限过滤可见 Worker
//! - `ensure_js_result_permission` —— 检查 Result 级别权限
//! - `resolve_accessible_js_result_workers` —— 解析可访问的 Result Worker 列表

use ng_core::error::NodegetError;
use ng_core::permission::data_structure::{
    JsResult as JsResultPermission, JsWorker as JsWorkerPermission, NodeGet, Permission, Scope,
    Token,
};
use ng_core::permission::token_auth::TokenOrAuth;
use ng_db::entity::js_result;
use sea_orm::{EntityTrait, QueryOrder, QuerySelect};
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use tracing::{trace, warn};

// ── TokenPermissionChecker trait + 全局注入 ────────────────────────

/// Token 权限校验 trait，由 server crate 实现并通过 [`set_token_checker`] 注入。
pub trait TokenPermissionChecker: Send + Sync + 'static {
    /// 检查 token/auth 是否满足给定的 scope 和 permission。
    fn check_token_limit(
        &self,
        token_or_auth: &TokenOrAuth,
        scopes: Vec<Scope>,
        permissions: Vec<Permission>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>>;

    /// 检查 token/auth 是否为超级 Token。
    fn check_super_token(
        &self,
        token_or_auth: &TokenOrAuth,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>>;

    /// 获取 token/auth 对应的 Token 元数据。
    fn get_token(
        &self,
        token_or_auth: &TokenOrAuth,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Token>> + Send + '_>>;
}

static TOKEN_CHECKER: OnceLock<Box<dyn TokenPermissionChecker>> = OnceLock::new();

/// 设置全局 Token 权限校验器，必须在服务器启动时调用一次。
pub fn set_token_checker(checker: Box<dyn TokenPermissionChecker>) {
    let _ = TOKEN_CHECKER.set(checker);
}

/// 获取全局 Token 权限校验器。
///
/// 若未初始化则 panic —— 必须先调用 [`set_token_checker`]。
pub fn get_token_checker() -> &'static dyn TokenPermissionChecker {
    TOKEN_CHECKER
        .get()
        .expect("TokenPermissionChecker not initialized — call set_token_checker first")
        .as_ref()
}

// ── js_worker 权限校验辅助函数 ────────────────────────────────────

/// 检查 token 是否具有指定 Worker 的指定权限。
///
/// - `token` —— 完整 token 字符串（key:secret 或 username|password）
/// - `worker_name` —— Worker 名称
/// - `permission` —— 所需权限（Create/Read/Write/Delete/Run/...）
///
/// 内部步骤：
/// 1. 解析 token 为 `TokenOrAuth`
/// 2. 调用 `check_token_limit` 检查 `Scope::JsWorker(worker_name)` + `Permission::JsWorker(permission)`
/// 3. 不通过则返回 `PermissionDenied` 错误
pub async fn check_js_worker_permission(
    token: &str,
    worker_name: &str,
    permission: JsWorkerPermission,
) -> anyhow::Result<()> {
    trace!(target: "js_worker", worker_name = %worker_name, permission = ?permission, "checking js_worker permission");
    let checker = get_token_checker();
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let permission_name = format!("{permission:?}");
    let is_allowed = checker
        .check_token_limit(
            &token_or_auth,
            vec![Scope::JsWorker(worker_name.to_owned())],
            vec![Permission::JsWorker(permission)],
        )
        .await?;

    if is_allowed {
        return Ok(());
    }

    warn!(target: "js_worker", worker_name = %worker_name, permission = %permission_name, "permission denied");
    Err(NodegetError::PermissionDenied(format!(
        "Permission denied for js_worker '{worker_name}', required permission: {permission_name}"
    ))
    .into())
}

/// 检查 token 是否具有运行时池查看权限（`nodeget.get_rt_pool`）。
///
/// 此权限属于 `Scope::Global` + `Permission::NodeGet(GetRtPool)`。
pub async fn check_get_rt_pool_permission(token: &str) -> anyhow::Result<()> {
    trace!(target: "js_worker", "checking get_rt_pool permission");
    let checker = get_token_checker();
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let is_allowed = checker
        .check_token_limit(
            &token_or_auth,
            vec![Scope::Global],
            vec![Permission::NodeGet(NodeGet::GetRtPool)],
        )
        .await?;

    if is_allowed {
        return Ok(());
    }

    warn!(target: "js_worker", "get_rt_pool permission denied");
    Err(NodegetError::PermissionDenied(
        "Permission denied: missing nodeget.get_rt_pool permission".to_owned(),
    )
    .into())
}

/// 按列表权限过滤 Worker 名称，仅返回 token 有权查看的 Worker。
///
/// - `token` —— 完整 token 字符串
/// - `worker_names` —— 待过滤的 Worker 名称列表
///
/// 逐个检查 `ListAllJsWorker` 权限，返回允许的子集。
pub async fn filter_workers_by_list_permission(
    token: &str,
    worker_names: Vec<String>,
) -> anyhow::Result<Vec<String>> {
    trace!(target: "js_worker", count = worker_names.len(), "filtering workers by list permission");
    let checker = get_token_checker();
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let mut allowed = Vec::new();
    for worker_name in worker_names {
        let is_allowed = checker
            .check_token_limit(
                &token_or_auth,
                vec![Scope::JsWorker(worker_name.clone())],
                vec![Permission::JsWorker(JsWorkerPermission::ListAllJsWorker)],
            )
            .await?;

        if is_allowed {
            allowed.push(worker_name);
        }
    }

    Ok(allowed)
}

// ── js_result 权限校验辅助函数 ────────────────────────────────────

/// JS Result 操作类型。
#[derive(Debug, Clone, Copy)]
pub enum JsResultAction {
    /// 读取结果
    Read,
    /// 删除结果
    Delete,
}

/// 根据 action 和 worker_name 构建所需的 Permission。
fn build_required_permission(action: JsResultAction, worker_name: &str) -> Permission {
    match action {
        JsResultAction::Read => {
            Permission::JsResult(JsResultPermission::Read(worker_name.to_owned()))
        }
        JsResultAction::Delete => {
            Permission::JsResult(JsResultPermission::Delete(worker_name.to_owned()))
        }
    }
}

/// 检查 token 是否具有指定 Worker 的 Result 操作权限。
///
/// - `token` —— 完整 token 字符串
/// - `worker_name` —— Worker 名称
/// - `action` —— 操作类型（Read/Delete）
pub async fn ensure_js_result_permission(
    token: &str,
    worker_name: &str,
    action: JsResultAction,
) -> anyhow::Result<()> {
    trace!(target: "js_result", worker_name = %worker_name, action = ?action, "checking js_result permission");
    let checker = get_token_checker();
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let is_allowed = checker
        .check_token_limit(
            &token_or_auth,
            vec![Scope::JsWorker(worker_name.to_owned())],
            vec![build_required_permission(action, worker_name)],
        )
        .await?;

    if is_allowed {
        return Ok(());
    }

    warn!(target: "js_result", worker_name = %worker_name, action = ?action, "permission denied");
    Err(NodegetError::PermissionDenied(format!(
        "Permission denied for js_result on worker '{worker_name}', action: {action:?}"
    ))
    .into())
}

/// 解析 token 可访问的 Result Worker 列表。
///
/// 从数据库查询所有 `js_result` 记录的 `js_worker_name`（去重），
/// 然后逐个检查权限，返回允许访问的子集。
///
/// - `token` —— 完整 token 字符串
/// - `action` —— 操作类型（Read/Delete）
pub async fn resolve_accessible_js_result_workers(
    token: &str,
    action: JsResultAction,
) -> anyhow::Result<Vec<String>> {
    trace!(target: "js_result", action = ?action, "resolving accessible js_result workers");
    let checker = get_token_checker();
    let token_or_auth = TokenOrAuth::from_full_token(token)
        .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

    let db = ng_db::get_db()
        .ok_or_else(|| NodegetError::DatabaseError("DB not initialized".to_owned()))?;

    // 查询所有 js_result 记录中的 js_worker_name 并去重
    let mut worker_names: Vec<String> = js_result::Entity::find()
        .select_only()
        .column(js_result::Column::JsWorkerName)
        .order_by_asc(js_result::Column::JsWorkerName)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| NodegetError::DatabaseError(e.to_string()))?;

    worker_names.dedup();

    // 逐个检查权限，保留允许的 Worker
    let mut allowed = Vec::new();
    for worker_name in worker_names {
        let is_allowed = checker
            .check_token_limit(
                &token_or_auth,
                vec![Scope::JsWorker(worker_name.clone())],
                vec![build_required_permission(action, worker_name.as_str())],
            )
            .await?;

        if is_allowed {
            allowed.push(worker_name);
        }
    }

    Ok(allowed)
}
