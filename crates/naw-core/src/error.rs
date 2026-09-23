//! Shared application error type.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The message names what is wrong so an operator can fix it, and must
    /// never quote a secret. HTTP responses still say only "internal error".
    #[error("configuration error: {0}")]
    Config(String),
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("cache error")]
    Valkey(#[from] deadpool_redis::PoolError),
    #[error("cache error")]
    ValkeyCommand(#[from] deadpool_redis::redis::RedisError),
    #[error("internal error")]
    Internal,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Details go to the log where they are safe, never to the response
        // body, and never include configuration values.
        let (status, message) = match &self {
            AppError::Config(_) | AppError::Internal => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
            AppError::Database(err) => {
                tracing::error!(error = %err, "database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
            AppError::Valkey(err) => {
                tracing::error!(error = %err, "cache error");
                (StatusCode::SERVICE_UNAVAILABLE, "cache unavailable")
            }
            AppError::ValkeyCommand(err) => {
                tracing::error!(error = %err, "cache command error");
                (StatusCode::SERVICE_UNAVAILABLE, "cache unavailable")
            }
        };
        (
            status,
            Json(json!({ "status": status.as_u16(), "message": message })),
        )
            .into_response()
    }
}
