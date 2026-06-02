//! Agent 自更新任务模块。
//!
//! 根据 Server 下发的版本 tag，检查是否需要更新、下载新二进制并替换当前可执行文件。
//! 更新成功后由 `tasks/mod.rs` 触发进程重启。

use ng_core::self_update::{canonical_exe_path, check_if_update_needed, get_url, replace_binary};

/// 执行 Agent 自更新。
///
/// - `tag` - 目标版本 tag（如 "v0.5.2"）
///
/// 1. 获取当前可执行文件路径
/// 2. 比较当前版本与目标版本，判断是否需要更新
/// 3. 构建下载 URL 并下载新二进制
/// 4. 校验下载文件大小（小于 1KB 视为异常）
/// 5. 替换二进制文件
/// 6. Unix 上设置可执行权限 (0o755)
///
/// 返回 `true` 表示更新成功；任何步骤失败返回 `false`（不终止进程）。
pub async fn self_update(tag: &str) -> bool {
    // 之前这里 `canonical_exe_path` 返回 None 时直接 `std::process::exit(1)`，
    // 但 `self_update` 是 server 下发的一条 task；一条任务的前置检查失败
    // 却把整个 agent 杀掉会导致所有 server 连接全部掉线 / reload 无法进行，
    // 非常不合比例。现在失败走正常 task 失败路径（返回 false），让
    // `tasks/mod.rs` 上报 error TaskEventResponse。
    let Some(current) = canonical_exe_path() else {
        log::error!("Failed to get canonical exe path");
        return false;
    };

    let (current_version, target_version, should_update) = check_if_update_needed(tag);

    if should_update {
        log::info!(
            "Updating from version {}.{}.{} to {}.{}.{}",
            current_version.0,
            current_version.1,
            current_version.2,
            target_version.0,
            target_version.1,
            target_version.2
        );
    } else {
        log::info!(
            "Current version {}.{}.{} is up to date with target version {}.{}.{}",
            current_version.0,
            current_version.1,
            current_version.2,
            target_version.0,
            target_version.1,
            target_version.2
        );
        return false;
    }

    let Some(url) = get_url(tag) else {
        log::error!("Failed to get download URL for tag: {tag}");
        return false;
    };

    log::info!("Downloading update from {url}");

    let client = reqwest::Client::new();
    let response = match client
        .get(&url)
        .header("User-Agent", "NodeGet-Agent")
        .timeout(std::time::Duration::from_mins(1))
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("Download request failed: {e}");
            return false;
        }
    };

    if !response.status().is_success() {
        log::error!("Download failed with status: {}", response.status());
        return false;
    }

    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            log::error!("Failed to read response body: {e}");
            return false;
        }
    };

    if bytes.len() < 1024 {
        log::error!(
            "Downloaded file too small ({} bytes), aborting",
            bytes.len()
        );
        return false;
    }

    log::info!("Downloaded {} bytes", bytes.len());

    if replace_binary(bytes.to_vec()) {
        log::info!("Binary replaced successfully: {}", current.display());
    } else {
        log::error!("Failed to replace binary");
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        if let Err(e) = std::fs::set_permissions(&current, perms) {
            log::warn!("Failed to set executable permission: {e}");
        }
    }

    true
}
