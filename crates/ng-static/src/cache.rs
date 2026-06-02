//! 静态文件桶内存缓存模块。
//!
//! 职责：将 `static_file` 表全部加载到内存，按 `name` 索引，
//! 并跟踪唯一一个 `is_http_root` 桶名（用作 HTTP 根路径回退）。
//!
//! 协作关系：通过 [`make_global_cache!`] 宏生成 `OnceLock` 全局单例，
//! 服务器启动时调用 `init()`，RPC handler 和 HTTP router 通过
//! `global()` 查询；增删改操作后调用 `reload()` 刷新缓存。

use ng_db::entity::static_file as static_entity;
use ng_infra::make_global_cache;
use ng_infra::server::{DbBackedCache, load_from_db};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};
use tracing::warn;

/// 缓存条目：持有 Arc 引用，可在读锁释放后安全使用。
pub struct CachedStatic {
    /// 数据库行模型，Arc 共享以降低 clone 开销。
    pub model: Arc<static_entity::Model>,
}

/// 缓存内部状态，被 `RwLock` 保护。
struct StaticCacheInner {
    /// 按 bucket `name` 索引的缓存条目。
    by_name: HashMap<String, CachedStatic>,
    /// 唯一标记为 `is_http_root` 的桶名，用于 HTTP 根路径回退。
    http_root_name: Option<String>,
}

/// 静态文件桶全量内存缓存，基于 `DbBackedCache` trait。
///
/// 通过 [`make_global_cache!`] 生成全局单例的 `init()` / `global()` / `reload()` 方法。
pub struct StaticCache {
    /// 读写锁保护内部 HashMap，允许并发读、独占写。
    inner: RwLock<StaticCacheInner>,
}

/// 从 RwLock 获取读锁，遇到 poison 时恢复而非 panic。
///
/// 原因：缓存更新线程 panic 不应阻塞后续所有读操作。
fn recover_read(
    lock: &RwLock<StaticCacheInner>,
) -> std::sync::RwLockReadGuard<'_, StaticCacheInner> {
    lock.read().unwrap_or_else(|e| {
        tracing::warn!(target: "static_cache", "lock poisoned during read, recovering");
        e.into_inner()
    })
}

/// 从 RwLock 获取写锁，遇到 poison 时恢复而非 panic。
fn recover_write(
    lock: &RwLock<StaticCacheInner>,
) -> std::sync::RwLockWriteGuard<'_, StaticCacheInner> {
    lock.write().unwrap_or_else(|e| {
        tracing::warn!(target: "static_cache", "lock poisoned during write, recovering");
        e.into_inner()
    })
}

// 生成全局单例：StaticCache::init() / StaticCache::global() / StaticCache::reload()
make_global_cache!(StaticCache, STATIC_CACHE_GLOBAL);

impl DbBackedCache for StaticCache {
    type Model = static_entity::Model;

    /// 缓存标识名，用于日志输出。
    fn cache_name() -> &'static str {
        "static_file"
    }

    /// 从数据库行构建缓存实例，内部调用 [`build_maps`]。
    fn build_cache(models: Vec<Self::Model>) -> Self {
        let (by_name, http_root_name) = Self::build_maps(models);
        Self {
            inner: RwLock::new(StaticCacheInner {
                by_name,
                http_root_name,
            }),
        }
    }

    /// 基于新的数据库行就地刷新缓存内容。
    ///
    /// 获取写锁 -> 替换内部 HashMap -> 主动 drop 写锁以减少持锁时间。
    #[allow(clippy::unused_async)]
    async fn reload_from_models(&self, models: Vec<Self::Model>) {
        let (by_name, http_root_name) = Self::build_maps(models);
        let mut guard = recover_write(&self.inner);
        guard.by_name = by_name;
        guard.http_root_name = http_root_name;
        drop(guard);
    }

    /// 从数据库全量加载 `static_file` 表。
    fn load_all() -> impl Future<Output = anyhow::Result<Vec<Self::Model>>> + Send {
        load_from_db::<static_entity::Entity>()
    }
}

impl StaticCache {
    /// 将数据库行列表转换为按 name 索引的 HashMap，并提取 `is_http_root` 桶名。
    ///
    /// 当遇到重复的 `is_http_root` 时保留首个，后续的记 warn 日志并忽略。
    fn build_maps(
        models: Vec<static_entity::Model>,
    ) -> (HashMap<String, CachedStatic>, Option<String>) {
        let mut by_name = HashMap::with_capacity(models.len());
        let mut http_root_name: Option<String> = None;

        for model in models {
            // is_http_root 最多只能有一个；重复时忽略并记录警告
            if model.is_http_root {
                if http_root_name.is_none() {
                    http_root_name = Some(model.name.clone());
                } else {
                    warn!(
                        target: "static",
                        name = %model.name,
                        existing = %http_root_name.as_ref().unwrap_or(&"unknown".to_string()),
                        "duplicate is_http_root detected, ignoring"
                    );
                }
            }
            let name = model.name.clone();
            by_name.insert(
                name,
                CachedStatic {
                    model: Arc::new(model),
                },
            );
        }

        (by_name, http_root_name)
    }

    /// 按 bucket `name` 查询缓存，返回 Arc 引用。
    pub fn get_by_name(&self, name: &str) -> Option<Arc<static_entity::Model>> {
        recover_read(&self.inner)
            .by_name
            .get(name)
            .map(|c| Arc::clone(&c.model))
    }

    /// 获取标记为 `is_http_root` 的桶模型，用于 HTTP 根路径回退。
    pub fn get_http_root(&self) -> Option<Arc<static_entity::Model>> {
        let guard = recover_read(&self.inner);
        let name = guard.http_root_name.as_ref()?;
        guard.by_name.get(name).map(|c| Arc::clone(&c.model))
    }

    /// 返回所有缓存条目的 Arc 引用列表。
    pub fn get_all(&self) -> Vec<Arc<static_entity::Model>> {
        recover_read(&self.inner)
            .by_name
            .values()
            .map(|c| Arc::clone(&c.model))
            .collect()
    }

    /// 判断指定 `name` 的桶是否存在于缓存中。
    pub fn exists(&self, name: &str) -> bool {
        recover_read(&self.inner).by_name.contains_key(name)
    }
}
