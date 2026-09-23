//! NotAnotherWiki Engine HTTP layer: router, middleware, handlers.

mod account;
mod admin;
mod audit;
mod auth;
pub mod bootstrap;
mod display_name;
mod errors;
mod history;
mod lang;
mod net;
mod observe;
mod pages;
mod perm;
mod policy;
mod profile;
mod resolve;
mod search;
mod settings;

/// Account credentials for the command line: password hashing, temporary
/// passwords and the username rule. Exposed on their own so the CLI can create
/// the first account without the rest of the auth module becoming public.
pub mod credentials {
    pub use crate::auth::password::{hash, temporary};
    pub use crate::auth::username::is_valid as username_is_valid;
}

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
        .route("/login", get(account::login_page))
        .route("/login/password", post(account::password_login))
        .route(
            "/settings/password",
            get(account::password_page).post(account::change_password),
        )
        // The first address of the password page, kept so an old link or a
        // bookmark still lands somewhere.
        .route(
            "/account/password",
            get(|| async { axum::response::Redirect::permanent("/settings/password") }),
        )
        .route("/logout", post(auth::routes::logout))
        .route("/settings", get(settings::page))
        .route("/settings/language", post(settings::set_language))
        .route("/settings/username", post(settings::change_username))
        .route("/settings/display-name", post(settings::set_display_name))
        .route(
            "/settings/sessions/end-others",
            post(settings::end_other_sessions),
        )
        .route("/auth/dev", get(auth::routes::dev_login))
        // The dev route is declared first so it wins over the generic
        // `{provider}` match below.
        .route("/auth/{provider}", get(auth::routes::start))
        .route("/auth/{provider}/callback", get(auth::routes::callback))
        // The admin panel. Every mutation is a POST, which is what makes
        // SameSite=Lax the CSRF defence for this whole subtree. See admin.rs.
        .route("/admin", get(admin::overview))
        .route("/admin/users", get(admin::users))
        .route("/admin/users/role", post(admin::set_role))
        .route(
            "/admin/users/new",
            get(admin::new_user).post(admin::create_user),
        )
        .route("/admin/users/password", post(admin::reset_password))
        .route("/admin/pages", get(admin::pages_list))
        .route("/admin/pages/{action}", post(admin::page_action))
        .route("/admin/audit", get(admin::audit_log))
        .route(
            "/admin/wiki",
            get(admin::wiki_settings).post(admin::save_wiki_settings),
        )
        .route("/admin/reindex", post(admin::reindex))
        .route(
            "/admin/accounts",
            get(admin::account_rules).post(admin::save_account_rules),
        )
        .route(
            "/admin/languages",
            get(admin::languages).post(admin::save_languages),
        )
        .route("/admin/reload", post(admin::reload))
        .route("/admin/errors", get(admin::error_gallery))
        .route("/admin/errors/{kind}", get(admin::error_preview))
        .route("/search", get(search::search_page))
        .route("/user/{name}", get(profile::show))
        .route("/user/{name}/edit", get(profile::edit).post(profile::save))
        .route("/lang", post(pages::set_language))
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
        .route("/{slug}/history", get(history::history))
        .route("/{slug}/diff", get(history::diff))
        .route("/{slug}/rev/{revision}", get(history::revision))
        .route("/{slug}/revert", post(history::revert))
        .route("/{slug}/patrol", post(history::patrol))
        // Declared before `.layer` on purpose: axum only wraps what already
        // exists, and a fallback added afterwards would skip every middleware,
        // including the one that turns a bare 404 into a page.
        .fallback(pages::fallback)
        .layer(
            tower::ServiceBuilder::new()
                .layer(tower_http::catch_panic::CatchPanicLayer::new())
                // Outermost after the panic guard, so even a request that dies
                // downstream still gets an id in its response and its log line.
                .layer(axum::middleware::from_fn(observe::layer))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    auth::session::layer,
                ))
                // Inside the session layer, because an error page shows who is
                // signed in; outside the body limit, so a 413 is themed too.
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    errors::layer,
                ))
                // Inside the session layer: a language switch never needs a
                // session, and this way it answers before any handler runs.
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    lang::layer,
                ))
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
