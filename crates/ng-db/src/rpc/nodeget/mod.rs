//! `nodeget-server` 命名空间下的数据库相关 RPC
//!
//! 包含主库管理类方法：存储占用查询、主库 SQL 执行、数据库类型查询。
//! 与 `db` 命名空间不同，这里操作的是主库而非用户创建的子数据库。

pub mod database_storage;
pub mod exec_sql;
pub mod get_database_type;
