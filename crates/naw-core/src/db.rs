//! Database and cache pool construction plus migrations.

use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::error::AppError;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

pub async fn connect(database_url: &str) -> Result<PgPool, AppError> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
        .map_err(AppError::Database)
}

pub async fn connect_valkey(valkey_url: &str) -> Result<deadpool_redis::Pool, AppError> {
    let config = deadpool_redis::Config::from_url(valkey_url);
    config
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        // The pool error can quote the URL, and a Valkey URL can carry a
        // password. The message says which setting to look at instead.
        .map_err(|_| {
            AppError::Config("NAW_VALKEY_URL could not be used to build a pool".to_string())
        })
}

pub async fn run_migrations(pool: &PgPool) -> Result<(), AppError> {
    MIGRATOR
        .run(pool)
        .await
        .map_err(|err| AppError::Database(sqlx::Error::Migrate(Box::new(err))))
}
