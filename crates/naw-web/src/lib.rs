//! NotAnotherWiki Engine HTTP layer: router, middleware, handlers.

mod pages;
mod resolve;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use naw_core::error::AppError;
use naw_core::state::AppState;
use serde_json::json;
use tracing::instrument;

/// Builds the application router.
///
/// Two global guards keep malformed traffic from ever reaching a handler:
/// oversized bodies are rejected with 413 before buffering, and a handler
/// panic becomes a plain 500 instead of a dropped connection. Responses
/// carry `nosniff` so browsers never reinterpret a body against its
/// content type.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/", get(pages::home))
        .route("/new", get(pages::new_page).post(pages::create_page))
        .route("/preview", post(pages::preview))
        .route("/favicon.ico", get(pages::favicon_ico))
        .route("/favicon-96x96.png", get(pages::favicon_png))
        .route("/apple-touch-icon.png", get(pages::apple_touch_icon))
        .route("/site.webmanifest", get(pages::site_manifest))
        .route(
            "/web-app-manifest-192x192.png",
            get(pages::manifest_icon_192),
        )
        .route(
            "/web-app-manifest-512x512.png",
            get(pages::manifest_icon_512),
        )
        .route("/{slug}", get(pages::page))
        .route("/{slug}/edit", get(pages::edit_page).post(pages::save_page))
        .layer(
            tower::ServiceBuilder::new()
                .layer(tower_http::catch_panic::CatchPanicLayer::new())
                .layer(tower_http::limit::RequestBodyLimitLayer::new(1024 * 1024))
                .layer(
                    tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                        axum::http::header::X_CONTENT_TYPE_OPTIONS,
                        axum::http::HeaderValue::from_static("nosniff"),
                    ),
                )
                .layer(tower_http::trace::TraceLayer::new_for_http()),
        )
        .with_state(state)
}

/// Liveness: the process is up. No database, no cache.
#[instrument]
async fn health() -> &'static str {
    "ok"
}

/// Readiness: deep check. One compile-checked query and one cache PING.
#[instrument(skip(state))]
async fn ready(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query!("SELECT 1 AS one").fetch_one(&state.db).await?;
    let mut conn = state.valkey.get().await?;
    let pong: String = deadpool_redis::redis::cmd("PING")
        .query_async(&mut conn)
        .await?;
    let db_ok = row.one == Some(1);
    let cache_ok = pong == "PONG";
    let ok = db_ok && cache_ok;
    Ok(Json(json!({
        "status": if ok { "ok" } else { "degraded" },
        "db": db_ok,
        "cache": cache_ok
    })))
}
