//! `crontab` RPC 命名空间实现：定时任务的 CRUD 与启用/禁用操作。
//!
//! 使用 jsonrpsee `#[rpc]` 宏定义 trait，`CrontabRpcImpl` 实现各方法，
//! 每个方法通过 `rpc_exec!` 宏统一日志与错误处理，
//! 具体业务逻辑委托到子模块（create、edit、delete、get、set_enable）。

use crate::CronType;
use jsonrpsee::core::RpcResult;
use jsonrpsee::core::async_trait;
use jsonrpsee::proc_macros::rpc;
use ng_infra::rpc_exec;
use ng_infra::server::{RpcHelper, token_identity};
use serde_json::value::RawValue;
use tracing::Instrument;

mod auth;
mod create;
mod delete;
mod edit;
mod get;
mod set_enable;

/// `crontab` RPC trait 定义，使用 jsonrpsee `#[rpc]` 宏自动生成 Server 端调度代码。
/// 命名空间为 `crontab`，分隔符为 `_`（自定义 jsonrpsee fork）。
#[rpc(server, namespace = "crontab")]
pub trait Rpc {
    /// 创建定时任务
    #[method(name = "create")]
    async fn create(
        &self,
        token: String,
        name: String,
        cron_expression: String,
        cron_type: CronType,
    ) -> RpcResult<Box<RawValue>>;

    /// 编辑定时任务
    #[method(name = "edit")]
    async fn edit(
        &self,
        token: String,
        name: String,
        cron_expression: String,
        cron_type: CronType,
    ) -> RpcResult<Box<RawValue>>;

    /// 获取定时任务列表
    #[method(name = "get")]
    async fn get(&self, token: String) -> RpcResult<Box<RawValue>>;

    /// 删除定时任务
    #[method(name = "delete")]
    async fn delete(&self, token: String, name: String) -> RpcResult<Box<RawValue>>;

    /// 设置定时任务启用/禁用状态
    #[method(name = "set_enable")]
    async fn set_enable(
        &self,
        token: String,
        name: String,
        enable: bool,
    ) -> RpcResult<Box<RawValue>>;
}

/// `crontab` RPC 实现结构体，空载体（所有状态通过全局单例获取）。
pub struct CrontabRpcImpl;

impl RpcHelper for CrontabRpcImpl {}

#[async_trait]
impl RpcServer for CrontabRpcImpl {
    async fn create(
        &self,
        token: String,
        name: String,
        cron_expression: String,
        cron_type: CronType,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "crontab", "crontab::create", token_key = tk, username = un, name = %name, cron_expression = %cron_expression, cron_type = ?cron_type);
        async { rpc_exec!(create::create(token, name, cron_expression, cron_type).await) }
            .instrument(span)
            .await
    }

    async fn edit(
        &self,
        token: String,
        name: String,
        cron_expression: String,
        cron_type: CronType,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "crontab", "crontab::edit", token_key = tk, username = un, name = %name, cron_expression = %cron_expression, cron_type = ?cron_type);
        async { rpc_exec!(edit::edit(token, name, cron_expression, cron_type).await) }
            .instrument(span)
            .await
    }

    async fn get(&self, token: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span =
            tracing::info_span!(target: "crontab", "crontab::get", token_key = tk, username = un);
        async { rpc_exec!(get::get(token).await) }
            .instrument(span)
            .await
    }

    async fn delete(&self, token: String, name: String) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "crontab", "crontab::delete", token_key = tk, username = un, name = %name);
        async { rpc_exec!(delete::delete(token, name).await) }
            .instrument(span)
            .await
    }

    async fn set_enable(
        &self,
        token: String,
        name: String,
        enable: bool,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "crontab", "crontab::set_enable", token_key = tk, username = un, name = %name, enable = enable);
        async { rpc_exec!(set_enable::set_enable(token, name, enable).await) }
            .instrument(span)
            .await
    }
}

/// 构建并返回 `crontab` RPC 模块。
pub fn rpc_module() -> jsonrpsee::RpcModule<CrontabRpcImpl> {
    CrontabRpcImpl.into_rpc()
}
