//! Auth HTTP routes: /login, /logout and the loopback dev provider.
//!
//! The provider round trip (start + callback) lands with T5/T8, when the
//! provider registry exists. The dev provider signs in end to end without
//! any secrets, which unblocks the whole session flow today.

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use std::net::SocketAddr;

use naw_core::state::AppState;

use super::redirect::safe_next;
use super::session;

fn is_loopback(addr: Option<&SocketAddr>) -> bool {
    addr.map(|a| a.ip().is_loopback()).unwrap_or(false)
}

fn not_found_page() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\" />\
         <title>NotAnotherWiki</title></head><body><p>Not found</p>\
         <p><a href=\"/\">Back to the wiki</a></p></body></html>",
    )
        .into_response()
}

/// GET /login: the sign-in page. Provider buttons render once real creds
/// exist; with only the dev provider the page says so. Never cached.
pub async fn login_page(State(state): State<AppState>, _headers: HeaderMap) -> Response {
    if !state.config.auth.enabled {
        return not_found_page();
    }
    let mut buttons = String::new();
    if state.config.auth.github.is_some() {
        buttons.push_str("<a class=\"btn\" href=\"/auth/github\">Sign in with GitHub</a> ");
    }
    if state.config.auth.discord.is_some() {
        buttons.push_str("<a class=\"btn\" href=\"/auth/discord\">Sign in with Discord</a> ");
    }
    if state.config.auth.telegram.is_some() {
        buttons.push_str("<a class=\"btn\" href=\"/auth/telegram\">Sign in with Telegram</a> ");
    }
    if state.config.auth.dev_login {
        buttons.push_str("<a class=\"btn\" href=\"/auth/dev?next=/\">Dev sign in</a> ");
    }
    if buttons.is_empty() {
        buttons = "<p>No sign-in providers are configured yet.</p>".to_string();
    }
    let wiki = "NotAnotherWiki";
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\" />\
         <title>Sign in | {wiki}</title></head><body>\
         <h1>Sign in to {wiki}</h1>{buttons}\
         <p><a href=\"/\">Back to the wiki</a></p></body></html>"
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// GET /auth/dev: loopback-only instant sign-in as the fixed `dev` user.
/// Refuses when the flag is off (404), when base_url is https (defense in
/// depth, the config check happens at startup too) or off loopback.
pub async fn dev_login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if !state.config.auth.enabled
        || !state.config.auth.dev_login
        || !is_loopback(Some(&addr))
        || state
            .config
            .auth
            .base_url
            .as_deref()
            .unwrap_or_default()
            .starts_with("https://")
    {
        return not_found_page();
    }
    let identity = super::types::Identity {
        provider: super::types::ProviderId::Dev,
        provider_user_id: "dev".to_string(),
        email: Some("dev@localhost".to_string()),
        display_name: Some("Dev".to_string()),
        avatar_url: None,
        email_verified: false,
        handle: "dev".to_string(),
        raw: serde_json::json!({"provider": "dev"}),
    };
    let outcome = match super::store::finish_login(&state, &identity, None, false).await {
        Ok(outcome) => outcome,
        Err(err) => return err.into_response(),
    };
    let user_id = match outcome {
        super::store::Outcome::Login(id)
        | super::store::Outcome::Register(id)
        | super::store::Outcome::Link(id) => id,
    };
    let created = session::create(&state, user_id, Some(addr.ip()), None).await;
    let session_id = match created {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(error = %err, "dev session create failed");
            return super::AuthError::Upstream("session create failed".to_string()).into_response();
        }
    };
    let secure = state
        .config
        .auth
        .base_url
        .as_deref()
        .unwrap_or_default()
        .starts_with("https://");
    let cookie = session::session_cookie(
        &session_id.to_string(),
        state.config.auth.session_ttl_hours,
        secure,
    );
    let next = safe_next(None);
    let mut response = Redirect::to(&next).into_response();
    if let Ok(value) = header::HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// POST /logout: delete the session row, clear the cookie, 303 to /.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(session_id) = session::session_id_from_headers(&headers) {
        session::delete(&state, session_id).await;
    }
    let secure = state
        .config
        .auth
        .base_url
        .as_deref()
        .unwrap_or_default()
        .starts_with("https://");
    let mut response = Redirect::to("/").into_response();
    if let Ok(value) = header::HeaderValue::from_str(&session::clear_cookie(secure)) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}
