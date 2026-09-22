//! Auth HTTP routes: `/login`, the provider round trip, `/logout` and the
//! loopback dev provider.
//!
//! The round trip is two handlers. `start` mints a single-use state plus a
//! PKCE pair, parks them in Valkey and bounces the browser to the provider.
//! `callback` takes that state back, lets the provider turn the code into an
//! `Identity`, and hands it to `store::finish_login`.
//!
//! Errors stay generic on the page and detailed in the log, and every response
//! here is `no-store` via the session layer.

use std::collections::BTreeMap;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use std::net::SocketAddr;

use naw_core::state::AppState;

use crate::resolve::{self, Chrome};

use super::redirect::safe_next;
use super::state_store::{self, Flow, FlowState};
use super::types::ProviderId;
use super::{pkce, providers, render, session};

fn is_loopback(addr: Option<&SocketAddr>) -> bool {
    addr.map(|a| a.ip().is_loopback()).unwrap_or(false)
}

/// `Secure` on the session cookie tracks the scheme of the public base URL.
fn secure_cookies(state: &AppState) -> bool {
    state
        .config
        .auth
        .base_url
        .as_deref()
        .unwrap_or_default()
        .starts_with("https://")
}

/// The exact string registered in each provider console.
///
/// Derived from `auth.base_url` rather than the request Host header on
/// purpose: a provider compares this against its own allow list, and letting a
/// client-supplied header shape it would be an open invitation.
fn callback_uri(state: &AppState, provider: ProviderId) -> Option<String> {
    let base = state.config.auth.base_url.as_deref()?.trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}/auth/{}/callback", provider.as_str()))
}

/// The brand for this request, or a neutral one when no wiki matches. Auth
/// pages must still render when the Host is unknown, so a failure here degrades
/// to the fallback name rather than to a 500.
async fn chrome_or_fallback(state: &AppState, headers: &HeaderMap) -> Chrome {
    match resolve::chrome(&state.db, headers).await {
        Ok(Some(chrome)) => chrome,
        Ok(None) => Chrome {
            wiki_id: uuid::Uuid::nil(),
            wiki_name: resolve::UNKNOWN_WIKI.to_string(),
            lang: resolve::UNKNOWN_LANG.to_string(),
        },
        Err(err) => {
            tracing::error!(error = %err, "wiki lookup failed on an auth route");
            Chrome {
                wiki_id: uuid::Uuid::nil(),
                wiki_name: resolve::UNKNOWN_WIKI.to_string(),
                lang: resolve::UNKNOWN_LANG.to_string(),
            }
        }
    }
}

/// GET /login: the sign-in page. Buttons come from the registry, so a provider
/// appears exactly when its credentials exist. Rendered by the active skin and
/// never cached.
pub async fn login_page(
    State(state): State<AppState>,
    Query(params): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let chrome = chrome_or_fallback(&state, &headers).await;
    if !state.config.auth.enabled {
        return crate::errors::not_found();
    }
    let next = safe_next(params.get("next").map(String::as_str));
    // `?err=` is set by our own redirects, so it selects a fixed string rather
    // than being echoed back into the page.
    let error = match params.get("err").map(String::as_str) {
        Some("cancelled") => Some("The last sign-in was cancelled. Nothing was created."),
        Some("expired") => Some("That sign-in link had expired. This one will work."),
        Some(_) => Some("The last sign-in did not go through. Try again."),
        None => None,
    };
    render::login(&state, &chrome, &next, error)
}

/// GET /auth/{provider}: mint state and PKCE, then 302 to the provider.
///
/// `?mode=link` attaches the new identity to the signed-in user instead of
/// logging in. It needs a session; without one it degrades to a plain login
/// rather than failing, because the only thing lost is the linking intent.
pub async fn start(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(params): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !state.config.auth.enabled {
        return crate::errors::not_found();
    }
    let Some(id) = ProviderId::from_slug(&slug) else {
        return crate::errors::not_found();
    };
    let Some(provider) = providers::resolve(&state.config.auth, id) else {
        return crate::errors::not_found();
    };
    let Some(redirect_uri) = callback_uri(&state, id) else {
        tracing::error!(
            provider = id.as_str(),
            "auth.base_url is unset, cannot build a callback URL"
        );
        return render::auth_error(&super::AuthError::Upstream(
            "callback url unavailable".to_string(),
        ));
    };

    let linking = params.get("mode").map(String::as_str) == Some("link");
    let user_id = if linking {
        session::load_from_cookie(&state, &headers)
            .await
            .map(|user| user.id)
    } else {
        None
    };

    let pair = pkce::generate();
    let nonce = super::random_token();
    let next = safe_next(params.get("next").map(String::as_str));

    let flow = FlowState {
        provider: id.as_str().to_string(),
        mode: if user_id.is_some() {
            Flow::Link
        } else {
            Flow::Login
        },
        verifier: pair.verifier.clone(),
        nonce: nonce.clone(),
        next,
        user_id,
    };
    let state_token = match state_store::begin(&state.valkey, flow).await {
        Ok(token) => token,
        Err(err) => return render::auth_error(&err),
    };

    let url = match provider.authorize_url(&providers::AuthorizeParams {
        redirect_uri: &redirect_uri,
        state: &state_token,
        code_challenge: Some(&pair.challenge),
        nonce: Some(&nonce),
    }) {
        Ok(url) => url,
        Err(err) => return render::auth_error(&err),
    };
    // 302 for the outbound leg, 303 after the callback. Keeps logs honest.
    (StatusCode::FOUND, [(header::LOCATION, url)]).into_response()
}

/// GET /auth/{provider}/callback: finish the round trip and sign the user in.
pub async fn callback(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !state.config.auth.enabled {
        return crate::errors::not_found();
    }
    let Some(id) = ProviderId::from_slug(&slug) else {
        return crate::errors::not_found();
    };
    let Some(provider) = providers::resolve(&state.config.auth, id) else {
        return crate::errors::not_found();
    };
    let Some(redirect_uri) = callback_uri(&state, id) else {
        return render::auth_error(&super::AuthError::Upstream(
            "callback url unavailable".to_string(),
        ));
    };

    // The state is taken before anything else and is single use, so a replayed
    // callback dies here rather than minting a second session.
    let Some(state_token) = query.get("state").filter(|token| !token.is_empty()) else {
        return render::auth_error(&super::AuthError::BadRequest("callback carried no state"));
    };
    let flow = match state_store::take(&state.valkey, state_token, id.as_str()).await {
        Ok(flow) => flow,
        Err(err) => return render::auth_error(&err),
    };

    let identity = match provider
        .complete(&providers::CompleteParams {
            query: &query,
            redirect_uri: &redirect_uri,
            code_verifier: Some(&flow.verifier),
            nonce: Some(&flow.nonce),
            cache: Some(&state.valkey),
            http: super::http::shared(),
        })
        .await
    {
        Ok(identity) => identity,
        Err(err) => return render::auth_error(&err),
    };

    let link_user_id = match flow.mode {
        Flow::Link => flow.user_id,
        Flow::Login => None,
    };
    let outcome = match super::store::finish_login(
        &state,
        &identity,
        link_user_id,
        state.config.auth.auto_link_verified_email,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(err) => return render::auth_error(&err),
    };
    let user_id = match outcome {
        super::store::Outcome::Login(id)
        | super::store::Outcome::Register(id)
        | super::store::Outcome::Link(id) => id,
    };

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let session_id = match session::create(&state, user_id, Some(addr.ip()), user_agent).await {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(error = %err, "session create failed after a provider login");
            return render::auth_error(&super::AuthError::Upstream(
                "session create failed".to_string(),
            ));
        }
    };

    sign_in_response(&state, &session_id.to_string(), &flow.next)
}

/// 303 to `next` with the session cookie attached.
fn sign_in_response(state: &AppState, session_value: &str, next: &str) -> Response {
    let cookie = session::session_cookie(
        session_value,
        state.config.auth.session_ttl_hours,
        secure_cookies(state),
    );
    let mut response = Redirect::to(next).into_response();
    if let Ok(value) = header::HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// GET /auth/dev: loopback-only instant sign-in as the fixed `dev` user.
/// Refuses when the flag is off (404), when base_url is https (defense in
/// depth, the config check happens at startup too) or off loopback.
pub async fn dev_login(
    State(state): State<AppState>,
    Query(params): Query<BTreeMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if !state.config.auth.enabled
        || !state.config.auth.dev_login
        || !is_loopback(Some(&addr))
        || secure_cookies(&state)
    {
        return crate::errors::not_found();
    }
    let identity = super::types::Identity {
        provider: ProviderId::Dev,
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
        Err(err) => {
            return render::auth_error(&err);
        }
    };
    let user_id = match outcome {
        super::store::Outcome::Login(id)
        | super::store::Outcome::Register(id)
        | super::store::Outcome::Link(id) => id,
    };
    let session_id = match session::create(&state, user_id, Some(addr.ip()), None).await {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(error = %err, "dev session create failed");
            return render::auth_error(&super::AuthError::Upstream(
                "session create failed".to_string(),
            ));
        }
    };
    let next = safe_next(params.get("next").map(String::as_str));
    sign_in_response(&state, &session_id.to_string(), &next)
}

/// POST /logout: delete the session row, clear the cookie, 303 to /.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(session_id) = session::session_id_from_headers(&headers) {
        session::delete(&state, session_id).await;
    }
    let mut response = Redirect::to("/").into_response();
    if let Ok(value) = header::HeaderValue::from_str(&session::clear_cookie(secure_cookies(&state)))
    {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use naw_core::config::{AuthConfig, Config, OAuth2Creds};

    fn state_with(auth: AuthConfig) -> Config {
        Config {
            auth,
            ..Config::default()
        }
    }

    #[test]
    fn callback_uri_is_built_from_config_not_from_a_header() {
        let cfg = state_with(AuthConfig {
            base_url: Some("https://snackers.wiki/".to_string()),
            ..AuthConfig::default()
        });
        // Trailing slash must not produce a double slash in the path.
        let base = cfg.auth.base_url.as_deref().unwrap().trim_end_matches('/');
        assert_eq!(
            format!("{base}/auth/{}/callback", ProviderId::Github.as_str()),
            "https://snackers.wiki/auth/github/callback"
        );
    }

    #[test]
    fn secure_flag_follows_the_base_url_scheme() {
        for (base, expected) in [
            (Some("https://snackers.wiki"), true),
            (Some("http://127.0.0.1:4242"), false),
            (None, false),
        ] {
            let cfg = state_with(AuthConfig {
                base_url: base.map(str::to_string),
                ..AuthConfig::default()
            });
            let secure = cfg
                .auth
                .base_url
                .as_deref()
                .unwrap_or_default()
                .starts_with("https://");
            assert_eq!(secure, expected, "{base:?}");
        }
    }

    #[test]
    fn the_login_page_lists_only_configured_providers() {
        let auth = AuthConfig {
            enabled: true,
            github: Some(OAuth2Creds {
                client_id: "id".to_string(),
                client_secret: "secret".to_string(),
            }),
            ..AuthConfig::default()
        };
        let labels: Vec<_> = providers::enabled(&auth)
            .iter()
            .map(|provider| provider.label())
            .collect();
        assert_eq!(labels, vec!["GitHub"]);
    }
}
