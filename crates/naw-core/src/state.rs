//! Application state shared by every handler.

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;

use crate::backends::{LocalStorage, NoopSearch, SearchBackend, StorageBackend};
use crate::config::Config;
use crate::db;
use crate::error::AppError;
use crate::skin::Skin;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub valkey: deadpool_redis::Pool,
    pub config: Arc<Config>,
    pub search: Arc<dyn SearchBackend>,
    pub storage: Arc<dyn StorageBackend>,
    /// Templates and messages, reloadable. Take one `current()` snapshot per request.
    pub skin: Arc<Skin>,
}

pub async fn build(config: Config) -> Result<AppState, AppError> {
    let db = db::connect(&config.database_url).await?;
    let valkey = db::connect_valkey(&config.valkey_url).await?;
    let storage: Arc<dyn StorageBackend> = Arc::new(LocalStorage::new(&config.storage_root)?);
    let skin = Arc::new(Skin::load(
        &config.skin_dir,
        crate::templates::DEFAULT_SKIN_DIR,
        &config.locales_dir,
    )?);
    if config.reload_interval_secs > 0 {
        Arc::clone(&skin).watch(Duration::from_secs(config.reload_interval_secs));
    }
    Ok(AppState {
        db,
        valkey,
        config: Arc::new(config),
        search: Arc::new(NoopSearch),
        storage,
        skin,
    })
}
