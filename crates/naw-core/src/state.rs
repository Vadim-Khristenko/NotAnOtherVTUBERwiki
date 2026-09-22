//! Shared application state wired from configuration.

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
    /// Templates and interface messages, reloadable at runtime. Take one
    /// snapshot per request with `skin.current()` and render from it; see
    /// `skin.rs` for why the two travel together.
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
    // Zero turns the watcher off, for an install that wants reloads only from
    // the admin panel. It is cheap enough to leave on everywhere else: a walk
    // over a few dozen files every couple of seconds.
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
