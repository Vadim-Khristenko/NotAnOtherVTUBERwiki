//! Shared application state wired from configuration.

use std::sync::Arc;

use sqlx::PgPool;

use crate::backends::{LocalStorage, NoopSearch, SearchBackend, StorageBackend};
use crate::config::Config;
use crate::db;
use crate::error::AppError;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub valkey: deadpool_redis::Pool,
    pub config: Arc<Config>,
    pub search: Arc<dyn SearchBackend>,
    pub storage: Arc<dyn StorageBackend>,
}

pub async fn build(config: Config) -> Result<AppState, AppError> {
    let db = db::connect(&config.database_url).await?;
    let valkey = db::connect_valkey(&config.valkey_url).await?;
    let storage: Arc<dyn StorageBackend> = Arc::new(LocalStorage::new(&config.storage_root)?);
    Ok(AppState {
        db,
        valkey,
        config: Arc::new(config),
        search: Arc::new(NoopSearch),
        storage,
    })
}
