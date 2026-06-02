//! `roll_super_token` 子命令
//!
//! 轮换 Super Token：删除当前 Super Token（id=1）并生成新的。
//! 需要用户交互确认，凭据仅输出到 stdout（不经过 tracing）。

use std::io::{self, Write};
use tracing::info;

use ng_token::super_token::roll_super_token;

/// 执行 Super Token 轮换
///
/// 内部步骤：
/// 1. 提示用户确认操作（会删除当前 Super Token）
/// 2. 调用 [`ng_token::super_token::roll_super_token`] 执行轮换
/// 3. 将新的 Token 和 Root Password 输出到 stdout（不经 tracing，避免泄漏到日志）
pub async fn run() {
    let should_continue = prompt_yes_or_no(
        "This action will delete the current super token (id=1) and generate a new one. Continue? [y/n]: ",
    );
    if !should_continue {
        info!(target: "server", "Super token rotation cancelled by user");
        return;
    }

    match roll_super_token().await {
        Ok((token, root_password)) => {
            info!(target: "server", "Super token rotated successfully");
            // 仅输出到 stdout——不经 tracing，
            // 因为 tracing 会将凭据写入 JSON 日志文件和内存缓冲区（可通过 RPC 查询）
            println!("Super Token: {token}");
            println!("Root Password: {root_password}");
        }
        Err(e) => {
            panic!("Failed to rotate super token: {e}");
        }
    }
}

/// 交互式 yes/no 确认提示
///
/// - prompt：提示文本
/// - 返回：用户输入 y/yes 时返回 true，n/no 时返回 false
/// - 输入无效时循环提示
fn prompt_yes_or_no(prompt: &str) -> bool {
    loop {
        print!("{prompt}");
        if let Err(e) = io::stdout().flush() {
            println!("Failed to flush stdout: {e}. Please type y or n.");
        }

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => {
                let normalized = input.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "y" | "yes" => return true,
                    "n" | "no" => return false,
                    _ => {
                        println!("Invalid input. Please type y or n.");
                    }
                }
            }
            Err(e) => {
                println!("Failed to read input: {e}. Please type y or n.");
            }
        }
    }
}
