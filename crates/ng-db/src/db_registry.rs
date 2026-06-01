use crate::entity::db_registry as dbreg_entity;
use crate::get_db;
use anyhow::Context;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, Database, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Notify;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

static MGR: std::sync::OnceLock<Arc<DbRegistryManager>> = std::sync::OnceLock::new();

struct TrackedConnection {
    conn: DatabaseConnection,
    last_used_ms: AtomicU64,
}

pub struct DbRegistryManager {
    db_path: String,
    pools: RwLock<HashMap<String, Arc<TrackedConnection>>>,
    cancelled: AtomicBool,
    cancel_notify: Notify,
    cleanup_handle: Mutex<Option<JoinHandle<()>>>,
}

fn now_ms_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

impl DbRegistryManager {
    pub async fn init(db_path: String) -> Arc<Self> {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let mgr_inner = Arc::new(Self {
                db_path,
                pools: RwLock::new(HashMap::new()),
                cancelled: AtomicBool::new(false),
                cancel_notify: Notify::new(),
                cleanup_handle: Mutex::new(None),
            });
            let mgr_clone = Arc::clone(&mgr_inner);
            let handle = tokio::spawn(async move {
                if let Err(e) = mgr_clone.seed_from_dbreg().await {
                    warn!(target: "db", error = %e, "Failed to seed db_registry from persisted state");
                }
                mgr_clone.start_cleanup_loop().await;
            });
            *mgr_inner.cleanup_handle.lock().unwrap() = Some(handle);
            let _ = MGR.set(mgr_inner);
        });
        Arc::clone(MGR.get().expect("DbRegistryManager not initialized"))
    }

    pub fn global() -> &'static Arc<Self> {
        MGR.get().expect("DbRegistryManager not initialized")
    }

    async fn seed_from_dbreg(&self) -> anyhow::Result<()> {
        let main_db = get_main_db()?;
        let entries = dbreg_entity::Entity::find().all(main_db).await?;
        let db_base = self.db_path.trim_end_matches('/');
        let mut pools = self.pools.write().await;
        for entry in entries {
            let name = &entry.name;
            let valid = !name.is_empty()
                && name.len() <= 128
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
                && name != "."
                && name != "..";
            if !valid {
                warn!(target: "db", name = %name, "Skipping db_registry entry with invalid name during seed");
                continue;
            }
            let db_url = format!("sqlite://{db_base}/{name}.db?mode=rwc");
            match Database::connect(&db_url).await {
                Ok(conn) => {
                    if conn.get_database_backend() == sea_orm::DatabaseBackend::Sqlite {
                        let _ = conn.execute_unprepared("PRAGMA journal_mode=WAL;").await;
                        let _ = conn.execute_unprepared("PRAGMA synchronous=NORMAL;").await;
                        let _ = conn.execute_unprepared("PRAGMA busy_timeout = 5000;").await;
                        let _ = conn.execute_unprepared("PRAGMA foreign_keys = ON;").await;
                    }
                    pools.insert(
                        name.clone(),
                        Arc::new(TrackedConnection {
                            conn,
                            last_used_ms: AtomicU64::new(now_ms_u64()),
                        }),
                    );
                    info!(target: "db", name = %name, "Restored database connection from registry");
                }
                Err(e) => {
                    error!(target: "db", name = %name, error = %e, "Failed to restore database connection");
                }
            }
        }
        Ok(())
    }

    async fn start_cleanup_loop(&self) {
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                info!(target: "db", "DbRegistry cleanup loop stopped");
                break;
            }
            tokio::select! {
                () = self.cancel_notify.notified() => {
                    info!(target: "db", "DbRegistry cleanup loop stopped");
                    break;
                }
                () = tokio::time::sleep(std::time::Duration::from_mins(1)) => {
                    if let Err(e) = self.cleanup_expired().await {
                        warn!(target: "db", error = %e, "DbRegistry cleanup failed, will retry next cycle");
                    }
                }
            }
        }
    }

    async fn cleanup_expired(&self) -> anyhow::Result<()> {
        let main_db = get_main_db()?;
        let to_remove = {
            let pools = self.pools.read().await;
            let mut expired = Vec::new();
            for (name, tracked) in pools.iter() {
                match dbreg_entity::Entity::find()
                    .filter(dbreg_entity::Column::Name.eq(name))
                    .one(main_db)
                    .await
                {
                    Ok(Some(m)) => {
                        if let Some(lifetime_ms) = m.max_lifetime_ms {
                            let last_used = tracked.last_used_ms.load(Ordering::Relaxed);
                            let elapsed_ms = now_ms_u64().saturating_sub(last_used) as i64;
                            if elapsed_ms >= lifetime_ms {
                                expired.push(name.clone());
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(target: "db", name = %name, error = %e, "Failed to query db_registry entry, skipping");
                    }
                }
            }
            expired
        };
        for name in to_remove {
            if let Err(e) = self.remove_conn(&name).await {
                warn!(target: "db", name = %name, error = %e, "Failed to remove expired connection");
            } else {
                info!(target: "db", name = %name, "Expired connection cleaned up");
            }
        }
        Ok(())
    }

    pub fn get_db_path(&self, name: &str) -> String {
        format!("{}/{}.db", self.db_path.trim_end_matches('/'), name)
    }

    pub async fn get_conn(&self, name: &str) -> Option<DatabaseConnection> {
        let pools = self.pools.read().await;
        pools.get(name).map(|tracked| {
            tracked.last_used_ms.store(now_ms_u64(), Ordering::Relaxed);
            tracked.conn.clone()
        })
    }

    pub async fn create_conn(
        &self,
        name: &str,
        max_lifetime_ms: Option<i64>,
    ) -> anyhow::Result<DatabaseConnection> {
        let db_url = format!(
            "sqlite://{}/{}.db?mode=rwc",
            self.db_path.trim_end_matches('/'),
            name
        );
        let conn = Database::connect(&db_url).await?;
        if conn.get_database_backend() == sea_orm::DatabaseBackend::Sqlite {
            let _ = conn
                .execute_unprepared(
                    "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;",
                )
                .await;
        }
        let now_ms = now_ms_u64() as i64;
        let main_db = get_main_db()?;
        let existing = dbreg_entity::Entity::find()
            .filter(dbreg_entity::Column::Name.eq(name))
            .one(main_db)
            .await?;
        if existing.is_none() {
            let active = dbreg_entity::ActiveModel {
                name: Set(name.to_owned()),
                db_connections: Set(Some(1)),
                max_lifetime_ms: Set(max_lifetime_ms),
                created_at: Set(now_ms),
                ..Default::default()
            };
            let result = active.insert(main_db).await?;
            info!(target: "db", name = %result.name, id = result.id, "Database registered");
        } else {
            let existing_model = existing.unwrap();
            let current_conns = existing_model.db_connections.unwrap_or(0).saturating_add(1);
            let mut active: dbreg_entity::ActiveModel = existing_model.into();
            active.db_connections = Set(Some(current_conns));
            if max_lifetime_ms.is_some() {
                active.max_lifetime_ms = Set(max_lifetime_ms);
            }
            active.update(main_db).await?;
        }
        {
            let mut pools = self.pools.write().await;
            pools.insert(
                name.to_owned(),
                Arc::new(TrackedConnection {
                    conn: conn.clone(),
                    last_used_ms: AtomicU64::new(now_ms_u64()),
                }),
            );
        }
        info!(target: "db", name = %name, "Database connection created");
        Ok(conn)
    }

    pub async fn remove_conn(&self, name: &str) -> anyhow::Result<()> {
        {
            let mut pools = self.pools.write().await;
            pools.remove(name);
        }
        let main_db = get_main_db()?;
        if let Some(model) = dbreg_entity::Entity::find()
            .filter(dbreg_entity::Column::Name.eq(name))
            .one(main_db)
            .await?
        {
            if let Err(e) = dbreg_entity::Entity::delete_by_id(model.id)
                .exec(main_db)
                .await
            {
                warn!(target: "db", name = %name, error = %e, "Failed to delete db_registry row");
            }
        }
        let db_file = self.get_db_path(name);
        if std::path::Path::new(&db_file).exists() {
            if let Err(e) = std::fs::remove_file(&db_file) {
                warn!(target: "db", path = %db_file, error = %e, "Failed to delete db file");
            }
        }
        for ext in &["-wal", "-shm"] {
            let f = format!("{db_file}{ext}");
            if std::path::Path::new(&f).exists() {
                if let Err(e) = std::fs::remove_file(&f) {
                    warn!(target: "db", path = %f, error = %e, "Failed to delete WAL/SHM file");
                }
            }
        }
        info!(target: "db", name = %name, "Database connection removed and files cleaned");
        Ok(())
    }

    pub async fn list_all(&self) -> anyhow::Result<Vec<DbInfo>> {
        let main_db = get_main_db()?;
        let entries = dbreg_entity::Entity::find()
            .order_by(dbreg_entity::Column::Name, sea_orm::Order::Asc)
            .all(main_db)
            .await?;
        let pools = self.pools.read().await;
        Ok(entries
            .iter()
            .map(|e| DbInfo {
                id: e.id,
                name: e.name.clone(),
                file_path: self.get_db_path(&e.name),
                db_connections: e.db_connections,
                max_lifetime_ms: e.max_lifetime_ms,
                created_at: e.created_at,
                is_active: pools.contains_key(&e.name),
            })
            .collect())
    }

    /// Signal the cleanup loop to stop and await its exit (with a 5-second timeout).
    ///
    /// # Panics
    ///
    /// Panics if the internal `Mutex` is poisoned (only possible if another holder panicked while locked).
    pub async fn shutdown(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.cancel_notify.notify_one();
        let handle = self.cleanup_handle.lock().unwrap().take();
        if let Some(handle) = handle {
            match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
                Ok(Ok(())) => info!(target: "db", "DbRegistry cleanup loop exited cleanly"),
                Ok(Err(e)) => warn!(target: "db", error = %e, "DbRegistry cleanup loop task panicked"),
                Err(_) => warn!(target: "db", "DbRegistry cleanup loop did not exit within 5s timeout"),
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbInfo {
    pub id: i64,
    pub name: String,
    pub file_path: String,
    pub db_connections: Option<i32>,
    pub max_lifetime_ms: Option<i64>,
    pub created_at: i64,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbExecResult {
    pub success: bool,
    pub data: Vec<serde_json::Value>,
    pub row_count: u64,
}

fn get_main_db() -> anyhow::Result<&'static sea_orm::DatabaseConnection> {
    get_db().context("Main DB not initialized")
}

pub fn row_to_json(r: &sea_orm::QueryResult) -> serde_json::Value {
    let cols = r.column_names();
    let mut map = serde_json::Map::with_capacity(cols.len());
    for col in cols {
        let val = try_column_as_json(r, &col);
        map.insert(col.clone(), val);
    }
    serde_json::Value::Object(map)
}

fn try_column_as_json(r: &sea_orm::QueryResult, col: &str) -> serde_json::Value {
    if let Ok(v) = r.try_get::<Option<String>>("", col) {
        return v.map_or(serde_json::Value::Null, serde_json::Value::String);
    }
    if let Ok(v) = r.try_get::<Option<i64>>("", col) {
        return v.map_or(serde_json::Value::Null, |n| {
            serde_json::Value::Number(n.into())
        });
    }
    if let Ok(v) = r.try_get::<Option<u32>>("", col) {
        return v.map_or(serde_json::Value::Null, |n| {
            serde_json::Value::Number(n.into())
        });
    }
    if let Ok(v) = r.try_get::<Option<f64>>("", col) {
        return v.map_or(serde_json::Value::Null, |n| {
            serde_json::Number::from_f64(n)
                .map_or(serde_json::Value::Null, serde_json::Value::Number)
        });
    }
    if let Ok(v) = r.try_get::<Option<bool>>("", col) {
        return v.map_or(serde_json::Value::Null, serde_json::Value::Bool);
    }
    if let Ok(v) = r.try_get::<Option<Vec<u8>>>("", col) {
        return match v {
            Some(bytes) => {
                if let Ok(j) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    j
                } else {
                    serde_json::Value::String(hex::encode(&bytes))
                }
            }
            None => serde_json::Value::Null,
        };
    }
    if let Ok(v) = r.try_get::<Option<serde_json::Value>>("", col) {
        return v.unwrap_or(serde_json::Value::Null);
    }
    serde_json::Value::Null
}

/// Convert a JSON value to a `SeaORM` `Value` for use as a SQL parameter.
/// This is shared between the db namespace RPC handlers.
#[must_use]
pub fn json_to_sea_value(json: &serde_json::Value) -> sea_orm::Value {
    match json {
        serde_json::Value::Null => sea_orm::Value::Json(None),
        serde_json::Value::Bool(b) => sea_orm::Value::Bool(Some(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                sea_orm::Value::BigInt(Some(i))
            } else if let Some(u) = n.as_u64() {
                sea_orm::Value::BigUnsigned(Some(u))
            } else if let Some(f) = n.as_f64() {
                sea_orm::Value::Double(Some(f))
            } else {
                sea_orm::Value::String(Some(n.to_string()))
            }
        }
        serde_json::Value::String(s) => sea_orm::Value::String(Some(s.clone())),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            sea_orm::Value::Json(Some(Box::new(json.clone())))
        }
    }
}

#[must_use]
pub fn is_read_query(sql: &str) -> bool {
    let s = sql.trim_start_matches(|c: char| c.is_whitespace() || c == '(' || c == ';');
    starts_with_ascii_ci(s, "SELECT")
        || starts_with_ascii_ci(s, "PRAGMA")
        || starts_with_ascii_ci(s, "EXPLAIN")
        || starts_with_ascii_ci(s, "WITH")
}

fn starts_with_ascii_ci(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .iter()
        .zip(prefix.as_bytes())
        .all(|(a, b)| a.to_ascii_uppercase() == *b)
}
