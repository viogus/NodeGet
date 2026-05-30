use sha2::{Digest, Sha256};

pub mod cache;
pub mod generate_token;
pub mod get;
pub mod super_token;

pub fn hash_string(need_hash: &str) -> String {
    let bytes = hash_to_bytes(need_hash);
    hex::encode(bytes)
}

pub fn hash_to_bytes(need_hash: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"NODEGET");
    hasher.update(need_hash.as_bytes());
    hasher.finalize().into()
}

use crate::DB;
use crate::entity::token;
use sea_orm::{ColumnTrait, DeleteResult, EntityTrait, QueryFilter};
use tracing::debug;

// 删除令牌的方法
//
// # 参数
// * `token_key` - 要删除的令牌的 key
//
// # 返回值
// 返回删除结果，包含删除的行数，或数据库错误
pub async fn delete_token_by_key(token_key: String) -> Result<DeleteResult, sea_orm::DbErr> {
    debug!(target: "token", %token_key, "Deleting token by key");
    let Some(db) = DB.get() else {
        return Err(sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
            "Database not initialized".to_owned(),
        )));
    };

    // 根据 token_key 删除令牌
    let delete_result = token::Entity::delete_many()
        .filter(token::Column::Id.ne(1))
        .filter(token::Column::TokenKey.eq(&token_key))
        .exec(db)
        .await?;

    if delete_result.rows_affected > 0 {
        debug!(target: "token", %token_key, rows_affected = delete_result.rows_affected, "Token deleted by key");
        if let Err(e) = cache::TokenCache::reload().await {
            tracing::error!(target: "token", error = %e, "Failed to reload token cache after delete_by_key");
        }
    } else {
        debug!(target: "token", %token_key, "No token found to delete by key");
    }

    Ok(delete_result)
}

// 根据用户名删除令牌的方法
//
// # 参数
// * `username` - 要删除的令牌的用户名
//
// # 返回值
// 返回删除结果，包含删除的行数，或数据库错误
pub async fn delete_token_by_username(username: String) -> Result<DeleteResult, sea_orm::DbErr> {
    debug!(target: "token", %username, "Deleting token by username");
    let Some(db) = DB.get() else {
        return Err(sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
            "Database not initialized".to_owned(),
        )));
    };

    // 根据用户名删除令牌
    let delete_result = token::Entity::delete_many()
        .filter(token::Column::Id.ne(1))
        .filter(token::Column::Username.eq(&username))
        .exec(db)
        .await?;

    if delete_result.rows_affected > 0 {
        debug!(target: "token", %username, rows_affected = delete_result.rows_affected, "Token deleted by username");
        if let Err(e) = cache::TokenCache::reload().await {
            tracing::error!(target: "token", error = %e, "Failed to reload token cache after delete_by_username");
        }
    } else {
        debug!(target: "token", %username, "No token found to delete by username");
    }

    Ok(delete_result)
}
