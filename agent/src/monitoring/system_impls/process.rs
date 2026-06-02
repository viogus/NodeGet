//! 进程数统计模块。
//!
//! 提供跨平台的进程计数功能：
//! - Windows：通过 `EnumProcesses` API 枚举
//! - Linux：遍历 `/proc` 目录，通过 `cmdline` 区分用户进程与内核线程
//! - 其他平台：返回 0 占位

/// 统计当前系统进程数（Windows 平台）。
///
/// 使用 `EnumProcesses` API 枚举进程 ID，按 MSDN 文档在返回结果饱和时加倍缓冲区重试。
///
/// 返回进程数；API 调用失败或缓冲区溢出 u32 时返回 0。
#[cfg(target_os = "windows")]
pub fn count_processes() -> u32 {
    use windows_sys::Win32::Foundation::FALSE;
    use windows_sys::Win32::System::ProcessStatus::EnumProcesses;

    // 初始缓冲区容量
    let mut cap: usize = 1024;

    loop {
        let mut bytes_returned: u32 = 0;
        let mut process_ids: Vec<u32> = vec![0; cap];

        let buf_bytes_u32: u32 = match u32::try_from(cap.saturating_mul(size_of::<u32>())) {
            Ok(v) => v,
            Err(_) => return 0, // cap 太大溢出 u32，放弃
        };

        let ok = unsafe {
            EnumProcesses(
                process_ids.as_mut_ptr(),
                buf_bytes_u32,
                &raw mut bytes_returned,
            )
        };

        if ok == FALSE {
            // EnumProcesses 在 buffer 小于实际需要时并**不会**返回
            // ERROR_INSUFFICIENT_BUFFER：它会成功地返回 `bytes_returned == buf_bytes_u32`，
            // 提示可能截断。因此这里的失败分支只处理真正的 API 错误，不再扩容。
            return 0;
        }

        // 当 bytes_returned 等于提供的 buffer 大小时，结果可能被截断，
        // 按 MSDN 文档加倍重试直到不再饱和。
        if bytes_returned == buf_bytes_u32 {
            cap = cap.saturating_mul(2);
            continue;
        }

        return bytes_returned / size_of::<u32>() as u32;
    }
}

/// 统计当前系统进程数（Linux 平台）。
///
/// 遍历 `/proc` 目录中名为数字 PID 的条目，通过读取 `/proc/<pid>/cmdline`
/// 区分用户进程与内核线程（内核线程的 `cmdline` 为空文件）。
/// 这与 `ps` / `procps` 使用相同启发式方法，开销低于解析 `/proc/<pid>/status`。
///
/// 返回用户进程数；`/proc` 不可读时返回 0。
#[cfg(target_os = "linux")]
pub fn count_processes() -> u32 {
    use std::fs;

    let Ok(entries) = fs::read_dir("/proc") else {
        return 0;
    };

    let mut count: u32 = 0;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if name.parse::<u32>().is_err() {
            continue;
        }

        // 读取 cmdline 与进程退出存在竞争；文件缺失或读取错误
        // 仅意味着"当前不是运行中的用户进程"，跳过即可。
        match fs::read(format!("/proc/{name}/cmdline")) {
            Ok(bytes) if !bytes.is_empty() => count += 1,
            _ => {}
        }
    }
    count
}

/// 统计当前系统进程数（其他平台）。
///
/// 目前尚未支持 macOS 等平台，返回 0 占位。
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub const fn count_processes() -> u32 {
    0 // TODO: MacOS Support
}
