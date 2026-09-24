//! NotAnotherWiki Engine HTTP layer: router, middleware, handlers.

/// The `Ok` value, or return the ready-made response in `Err` from the
/// handler: `let ctx = or_respond!(admin::gate(...).await);`.
macro_rules! or_respond {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(response) => return Ok(response),
        }
    };
}

mod account;
mod admin;
mod admin_user;
mod audit;
mod auth;
pub mod bootstrap;
mod chrome;
mod csp;
mod csrf;
mod diff;
mod display_name;
mod emotes;
mod errors;
mod fetch;
mod history;
pub mod indexing;
mod lang;
mod locale_path;
mod media;
mod net;
mod observe;
mod pages;
mod perm;
mod policy;
mod profile;
mod protect;
mod relay;
mod resolve;
mod search;
mod settings;
mod templates;
mod translate;

/// Credentials for the command line: password hashing, temporary passwords
/// and the username rule.
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

/// Builds the application router: body size limit, panic guard, request ids,
/// themed errors, sessions, `nosniff` and the CSP with a script nonce.
pub fn router(state: AppState) -> Router {
    // The language prefix comes off before routing (see locale_path.rs).
    let routes = routes(state.clone());
    Router::new().fallback_service(
        tower::ServiceBuilder::new()
            .layer(axum::middleware::from_fn_with_state(
                state,
                locale_path::layer,
            ))
            .service(routes),
    )
}

fn routes(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/login", get(account::login_page))
        .route("/login/password", post(account::password_login))
        .route("/login/relay", get(relay::page))
        .route("/login/relay/start", post(relay::start))
        .route(
            "/login/relay/approve",
            get(relay::approve_page).post(relay::approve),
        )
        .route("/login/relay/finish", post(relay::finish))
        .route(
            "/settings/password",
            get(account::password_page).post(account::change_password),
        )
        // The old address of the password page.
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
            "/settings/avatar",
            post(settings::set_avatar).layer(axum::extract::DefaultBodyLimit::max(
                state.config.avatar_max_bytes + 64 * 1024,
            )),
        )
        .route("/settings/avatar/remove", post(settings::remove_avatar))
        .route(
            "/settings/sessions/end-others",
            post(settings::end_other_sessions),
        )
        .route("/auth/dev", get(auth::routes::dev_login))
        // Before the generic `{provider}` route, so it wins.
        .route("/auth/{provider}", get(auth::routes::start))
        .route("/auth/{provider}/callback", get(auth::routes::callback))
        .route("/admin", get(admin::overview))
        .route("/admin/users", get(admin::users))
        .route("/admin/users/role", post(admin::set_role))
        .route(
            "/admin/users/new",
            get(admin::new_user).post(admin::create_user),
        )
        .route("/admin/users/password", post(admin::reset_password))
        .route("/admin/user/{name}", get(admin_user::show))
        .route(
            "/admin/user/{name}/capability",
            post(admin_user::set_capability),
        )
        .route(
            "/admin/user/{name}/sanction",
            post(admin_user::add_sanction),
        )
        .route(
            "/admin/user/{name}/lift/{id}",
            post(admin_user::lift_sanction),
        )
        .route("/admin/user/{name}/note", post(admin_user::add_note))
        .route("/admin/user/{name}/curator", post(admin_user::set_curator))
        .route(
            "/admin/user/{name}/verify-email",
            post(admin_user::verify_email),
        )
        .route(
            "/admin/user/{name}/avatar/remove",
            post(admin_user::remove_avatar),
        )
        .route(
            "/admin/user/{name}/end-sessions",
            post(admin_user::end_sessions),
        )
        .route("/admin/pages", get(admin::pages_list))
        .route("/admin/pages/{action}", post(admin::page_action))
        .route("/admin/audit", get(admin::audit_log))
        .route(
            "/admin/wiki",
            get(admin::wiki_settings).post(admin::save_wiki_settings),
        )
        .route("/admin/reindex", post(admin::reindex))
        .route(
            "/admin/chrome",
            get(admin::chrome_settings).post(admin::save_chrome_settings),
        )
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
        .route("/media", get(media::page))
        .route("/emotes", get(emotes::list))
        .route("/emotes.json", get(emotes::list_json))
        .route("/admin/emotes", get(emotes::admin_page))
        .route("/admin/emotes/add", post(emotes::add))
        .route("/admin/emotes/{id}/sync", post(emotes::resync))
        .route("/admin/emotes/{id}/remove", post(emotes::remove))
        .route(
            "/media/upload",
            post(media::upload).layer(axum::extract::DefaultBodyLimit::max(
                state.config.upload_max_bytes + 64 * 1024,
            )),
        )
        .route("/media/import", post(media::import_url))
        .route("/media/{prefix}/{file}", get(media::serve))
        .route("/user/{name}", get(profile::show))
        .route(
            "/user/{name}/edit",
            get(profile::edit).post(profile::save).layer(text_form()),
        )
        .route("/lang", post(pages::set_language))
        .route("/", get(pages::home))
        .route(
            "/new",
            get(pages::new_page)
                .post(pages::create_page)
                .layer(text_form()),
        )
        .route("/preview", post(pages::preview).layer(text_form()))
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
        .route(
            "/{slug}/edit",
            get(pages::edit_page)
                .post(pages::save_page)
                .layer(text_form()),
        )
        .route("/{slug}/history", get(history::history))
        .route(
            "/{slug}/translate",
            get(translate::form)
                .post(translate::create)
                .layer(text_form()),
        )
        .route("/{slug}/diff", get(history::diff))
        .route("/{slug}/rev/{revision}", get(history::revision))
        .route("/{slug}/revert", post(history::revert))
        .route("/{slug}/patrol", post(history::patrol))
        .route("/{slug}/protect", post(protect::set))
        // Before `.layer`: a fallback added after would skip every middleware.
        .fallback(pages::fallback)
        .layer(
            tower::ServiceBuilder::new()
                .layer(tower_http::catch_panic::CatchPanicLayer::new())
                // Outside everything that renders, so a page's nonce is the one its header allows.
                .layer(axum::middleware::from_fn(csp::layer))
                // Outermost after the panic guard, so every response gets an id.
                .layer(axum::middleware::from_fn(observe::layer))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    auth::session::layer,
                ))
                // Inside the session layer, so an error page shows who is signed in;
                // outside the body limit, so a 413 is themed too.
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    errors::layer,
                ))
                // Inside the error layer, so the refusal is a themed page.
                .layer(axum::middleware::from_fn(csrf::layer))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    lang::layer,
                ))
                // Sized for the largest single request, an image or an article form.
                // Other forms stop at axum's 2 MB default.
                .layer(tower_http::limit::RequestBodyLimitLayer::new(
                    state
                        .config
                        .upload_max_bytes
                        .max(state.config.avatar_max_bytes)
                        .max(pages::TEXT_FORM_MAX)
                        + 256 * 1024,
                ))
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

/// The body limit of the routes that carry an article.
fn text_form() -> axum::extract::DefaultBodyLimit {
    axum::extract::DefaultBodyLimit::max(pages::TEXT_FORM_MAX)
}

/// Liveness: the process is up.
#[instrument]
async fn health() -> &'static str {
    "ok"
}

/// Readiness: one query and one cache PING.
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
