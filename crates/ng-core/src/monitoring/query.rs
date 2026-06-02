//! 监控数据查询字段枚举
//!
//! `StaticDataQueryField` 与 `DynamicDataQueryField` 分别对应
//! 静态采集（5 分钟级）和动态采集（秒级）数据的可查询维度。
//! 每个变体提供数据库列名（`column_name`）与 JSON 键名（`json_key`）两种映射。

use serde::{Deserialize, Serialize};

/// 静态监控数据可查询字段
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum StaticDataQueryField {
    /// CPU 静态信息（型号、核心数等）
    Cpu,
    /// 系统静态信息（主机名、OS 等）
    System,
    /// GPU 静态信息（型号、显存等）
    Gpu,
}

impl StaticDataQueryField {
    /// 返回数据库中对应的列名。
    #[must_use]
    pub const fn column_name(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu_data",
            Self::System => "system_data",
            Self::Gpu => "gpu_data",
        }
    }

    /// 返回 JSON 输出中使用的键名。
    #[must_use]
    pub const fn json_key(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::System => "system",
            Self::Gpu => "gpu",
        }
    }
}

/// 动态监控数据可查询字段
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DynamicDataQueryField {
    /// CPU 使用率
    Cpu,
    /// 内存使用情况
    Ram,
    /// 系统负载
    Load,
    /// 系统级动态信息（运行时间等）
    System,
    /// 磁盘使用与 IO
    Disk,
    /// 网络流量与连接
    Network,
    /// GPU 使用率与显存
    Gpu,
}

impl DynamicDataQueryField {
    /// 返回数据库中对应的列名。
    #[must_use]
    pub const fn column_name(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu_data",
            Self::Ram => "ram_data",
            Self::Load => "load_data",
            Self::System => "system_data",
            Self::Disk => "disk_data",
            Self::Network => "network_data",
            Self::Gpu => "gpu_data",
        }
    }

    /// 返回 JSON 输出中使用的键名。
    #[must_use]
    pub const fn json_key(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Ram => "ram",
            Self::Load => "load",
            Self::System => "system",
            Self::Disk => "disk",
            Self::Network => "network",
            Self::Gpu => "gpu",
        }
    }
}
