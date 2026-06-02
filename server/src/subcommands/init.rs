//! `init` 子命令
//!
//! 初始化数据库并生成 Super Token。仅在首次部署时使用。

use tracing::info;

/// 执行初始化：生成 Super Token 并退出
///
/// 调用 [`super::init_or_skip_super_token`] 完成令牌生成，
/// 若数据库中已存在 Super Token 则跳过。
pub async fn run() {
    super::init_or_skip_super_token().await;
    info!(target: "server", "Initialization completed, exiting");
}
