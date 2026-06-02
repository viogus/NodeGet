//! 服务器子命令模块
//!
//! 包含 CLI 子命令的实现：
//! - `init`：初始化数据库并生成 Super Token
//! - `roll_super_token`：轮换 Super Token（删除旧的并生成新的）
//! - `serve`：启动 HTTP/WebSocket 服务器
//! - `get_uuid`：输出服务器 UUID

use tracing::info;

use ng_token::super_token::generate_super_token;

pub mod get_uuid;
pub mod init;
pub mod roll_super_token;
pub mod serve;

/// 初始化 Super Token（若尚未存在则生成，否则跳过）
///
/// 生成成功时输出 Token 和 Root Password 到日志。
/// 该函数被 `init` 和 `serve` 子命令共同调用。
async fn init_or_skip_super_token() {
    let token = match generate_super_token().await {
        Ok(token) => token,
        Err(e) => {
            panic!("Failed to generate super token: {e}");
        }
    };

    match token {
        Some(token) => {
            info!(target: "server", "Super Token: {}", token.0);
            info!(target: "server", "Root Password: {}", token.1);
        }
        None => {
            info!(target: "server", "Super Token already exists, skipped");
        }
    }
}
