//! Signing in through another device, with no JavaScript anywhere.
//!
//! For a browser that cannot finish a normal sign-in (JavaScript off, iPhone
//! Lockdown Mode, an in-app browser):
//!
//! 1. Device A starts a relay and shows a pairing code and a link. A cookie
//!    ties the relay to that browser.
//! 2. Device B, already signed in, opens the link, sees which device asks,
//!    confirms, and is shown a one-time token.
//! 3. A pastes the token and gets a session cookie.
//!
//! Codes and tokens are single use and live ten minutes; a relay allows five
//! wrong tokens, and starts and approvals are throttled. The server keeps
//! only digests of the relay secret and the token, in Valkey.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Extension, Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::redirect::{encode_component, safe_next};
use crate::auth::session::{self, CurrentUser};
use crate::auth::{random_token, routes::secure_cookies, token_hash};
use crate::pages::{ENGINE_VERSION, private_page, template_error};
use crate::resolve::Ctx;

const TTL_SECS: u64 = 600;
const TOKEN_TRIES: u32 = 5;
/// Relays one address may start, and approvals one account may try, per TTL.
const STARTS_PER_ADDRESS: i64 = 10;
const APPROVALS_PER_ACCOUNT: i64 = 20;
const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

#[derive(Serialize, Deserialize, Clone)]
struct Relay {
    /// Pairing code shown on device A.
    code: String,
    next: String,
    device: String,
    ip: String,
    created: i64,
    #[serde(default)]
    approved_by: Option<Uuid>,
    #[serde(default)]
    approver_name: Option<String>,
    #[serde(default)]
    token_digest: Option<String>,
    #[serde(default)]
    tries: u32,
}

fn cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-naw_relay"
    } else {
        "naw_relay"
    }
}

fn relay_key(secret: &str) -> String {
    format!("naw:relay:{}", hex::encode(token_hash(secret)))
}

fn code_key(code: &str) -> String {
    format!("naw:relay-code:{code}")
}

/// Eight characters without look-alikes, shown as `xxxx-xxxx`.
fn short_code() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let raw: String = (0..8)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect();
    format!("{}-{}", &raw[..4], &raw[4..])
}

/// A typed code in its stored form: lowercase, `xxxx-xxxx`, or `None`.
fn normalize(raw: &str) -> Option<String> {
    let chars: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    (chars.len() == 8 && chars.bytes().all(|b| ALPHABET.contains(&b)))
        .then(|| format!("{}-{}", &chars[..4], &chars[4..]))
}

fn digest(value: &str) -> String {
    hex::encode(token_hash(value))
}

async fn conn(state: &AppState) -> Result<deadpool_redis::Connection, AppError> {
    state.valkey.get().await.map_err(|err| {
        tracing::error!(error = %err, "valkey unavailable for sign-in relay");
        AppError::Internal
    })
}

async fn load(state: &AppState, key: &str) -> Result<Option<Relay>, AppError> {
    let mut c = conn(state).await?;
    let raw: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg(key)
        .query_async(&mut c)
        .await
        .map_err(|_| AppError::Internal)?;
    Ok(raw.and_then(|raw| serde_json::from_str(&raw).ok()))
}

async fn save(state: &AppState, key: &str, relay: &Relay, fresh: bool) -> Result<(), AppError> {
    let mut c = conn(state).await?;
    let payload = serde_json::to_string(relay).map_err(|_| AppError::Internal)?;
    let mut cmd = deadpool_redis::redis::cmd("SET");
    cmd.arg(key).arg(payload);
    if fresh {
        cmd.arg("EX").arg(TTL_SECS);
    } else {
        cmd.arg("KEEPTTL");
    }
    cmd.query_async::<()>(&mut c)
        .await
        .map_err(|_| AppError::Internal)
}

async fn forget(state: &AppState, keys: &[String]) {
    if let Ok(mut c) = conn(state).await {
        let _: Result<(), _> = deadpool_redis::redis::cmd("DEL")
            .arg(keys)
            .query_async(&mut c)
            .await;
    }
}

/// Counts one event under `key` for the TTL window; the count so far.
async fn bump(state: &AppState, key: &str) -> Result<i64, AppError> {
    let mut c = conn(state).await?;
    let (count,): (i64,) = deadpool_redis::redis::pipe()
        .cmd("INCR")
        .arg(key)
        .cmd("EXPIRE")
        .arg(key)
        .arg(TTL_SECS)
        .arg("NX")
        .ignore()
        .query_async(&mut c)
        .await
        .map_err(|_| AppError::Internal)?;
    Ok(count)
}

fn relay_secret(headers: &HeaderMap, secure: bool) -> Option<String> {
    let name = cookie_name(secure);
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, value)| *key == name && value.len() == 43)
        .map(|(_, value)| value.to_string())
}

fn relay_cookie(secret: &str, secure: bool, max_age: u64) -> Option<HeaderValue> {
    let mut cookie = format!(
        "{}={secret}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}",
        cookie_name(secure)
    );
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).ok()
}

fn minutes_ago(created: i64) -> i64 {
    ((chrono::Utc::now().timestamp() - created) / 60).max(0)
}

#[derive(Default)]
struct View<'a> {
    mode: &'a str,
    code: Option<&'a str>,
    token: Option<&'a str>,
    device: Option<&'a str>,
    ip: Option<&'a str>,
    minutes: i64,
    approver: Option<&'a str>,
    next: &'a str,
    error: Option<&'a str>,
    /// The public base URL, so the link for device B is complete.
    base: &'a str,
}

fn render(ctx: &Ctx, status: StatusCode, view: View<'_>) -> Result<Response, AppError> {
    let template = ctx
        .skin
        .env
        .get_template("relay.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("relay.title"),
                version => ENGINE_VERSION,
                mode => view.mode,
                code => view.code,
                token => view.token,
                device => view.device,
                device_ip => view.ip,
                minutes => view.minutes,
                approver => view.approver,
                next => view.next,
                approve_link => view.code.map(|c| format!("{}/login/relay/approve?code={}", view.base.trim_end_matches('/'), encode_component(c))),
                approve_base => format!("{}/login/relay/approve", view.base.trim_end_matches('/')),
                error => view.error.map(|key| ctx.t(&format!("relay.{key}"))),
            }
        })
        .map_err(template_error)?;
    Ok(private_page(status, html))
}

#[derive(Deserialize, Default)]
pub struct NextQuery {
    #[serde(default)]
    next: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    err: String,
}

/// GET /login/relay: device A. Offers to start, or shows the running relay.
pub async fn page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<NextQuery>,
) -> Result<Response, AppError> {
    if !state.config.auth.enabled {
        return Ok(crate::errors::not_found());
    }
    let ctx = or_respond!(crate::resolve::required(&state, &headers, None).await?);
    let next = safe_next(Some(query.next.as_str()));
    if user.is_some() {
        return Ok(Redirect::to(&next).into_response());
    }
    let error = match query.err.as_str() {
        "token" => Some("error_token"),
        "expired" => Some("error_expired"),
        "busy" => Some("error_busy"),
        _ => None,
    };
    let secure = secure_cookies(&state);
    if let Some(secret) = relay_secret(&headers, secure)
        && let Some(relay) = load(&state, &relay_key(&secret)).await?
    {
        return render(
            &ctx,
            StatusCode::OK,
            View {
                mode: "waiting",
                code: Some(&relay.code),
                base: state.config.auth.base_url.as_deref().unwrap_or(""),
                approver: relay.approver_name.as_deref(),
                next: &relay.next,
                error,
                ..View::default()
            },
        );
    }
    render(
        &ctx,
        StatusCode::OK,
        View {
            mode: "start",
            next: &next,
            error,
            ..View::default()
        },
    )
}

#[derive(Deserialize)]
pub struct StartForm {
    #[serde(default)]
    next: String,
}

/// POST /login/relay/start
pub async fn start(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<StartForm>,
) -> Result<Response, AppError> {
    if !state.config.auth.enabled {
        return Ok(crate::errors::not_found());
    }
    let ip = crate::net::client_ip(&headers, peer, state.config.trust_proxy);
    if bump(&state, &format!("naw:relay-start:{ip}")).await? > STARTS_PER_ADDRESS {
        return Ok(Redirect::to("/login/relay?err=busy").into_response());
    }
    let secret = random_token();
    let code = short_code();
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    let relay = Relay {
        code: code.clone(),
        next: safe_next(Some(form.next.as_str())),
        device: crate::settings::device_label(user_agent),
        ip: ip.to_string(),
        created: chrono::Utc::now().timestamp(),
        approved_by: None,
        approver_name: None,
        token_digest: None,
        tries: 0,
    };
    let key = relay_key(&secret);
    save(&state, &key, &relay, true).await?;
    let mut c = conn(&state).await?;
    deadpool_redis::redis::cmd("SET")
        .arg(code_key(&code))
        .arg(&key)
        .arg("EX")
        .arg(TTL_SECS)
        .query_async::<()>(&mut c)
        .await
        .map_err(|_| AppError::Internal)?;
    let mut response = Redirect::to("/login/relay").into_response();
    if let Some(cookie) = relay_cookie(&secret, secure_cookies(&state), TTL_SECS) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

#[derive(Deserialize)]
pub struct CodeForm {
    #[serde(default)]
    code: String,
}

/// GET /login/relay/approve?code=: device B, signed in. Shows who asks.
pub async fn approve_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<NextQuery>,
) -> Result<Response, AppError> {
    if !state.config.auth.enabled {
        return Ok(crate::errors::not_found());
    }
    let here = format!(
        "/login/relay/approve?code={}",
        encode_component(&query.code)
    );
    let Some(user) = user else {
        return Ok(
            Redirect::to(&format!("/login?next={}", encode_component(&here))).into_response(),
        );
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, Some(&user)).await?);
    let Some(code) = normalize(&query.code) else {
        let error = (!query.code.is_empty()).then_some("error_code");
        return render(
            &ctx,
            StatusCode::OK,
            View {
                mode: "enter",
                next: "/",
                error,
                ..View::default()
            },
        );
    };
    let relay = match find_by_code(&state, &code).await? {
        Some((_, relay)) if relay.approved_by.is_none() => relay,
        _ => {
            return render(
                &ctx,
                StatusCode::NOT_FOUND,
                View {
                    mode: "enter",
                    next: "/",
                    error: Some("error_code"),
                    ..View::default()
                },
            );
        }
    };
    render(
        &ctx,
        StatusCode::OK,
        View {
            mode: "confirm",
            code: Some(&code),
            device: Some(&relay.device),
            ip: Some(&relay.ip),
            minutes: minutes_ago(relay.created),
            next: "/",
            ..View::default()
        },
    )
}

async fn find_by_code(state: &AppState, code: &str) -> Result<Option<(String, Relay)>, AppError> {
    let mut c = conn(state).await?;
    let key: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg(code_key(code))
        .query_async(&mut c)
        .await
        .map_err(|_| AppError::Internal)?;
    let Some(key) = key else { return Ok(None) };
    Ok(load(state, &key).await?.map(|relay| (key, relay)))
}

/// POST /login/relay/approve: device B confirms and is shown the token.
pub async fn approve(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<CodeForm>,
) -> Result<Response, AppError> {
    if !state.config.auth.enabled {
        return Ok(crate::errors::not_found());
    }
    let Some(user) = user else {
        return Ok(Redirect::to("/login").into_response());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, Some(&user)).await?);
    let refuse = |status, error| {
        render(
            &ctx,
            status,
            View {
                mode: "enter",
                next: "/",
                error: Some(error),
                ..View::default()
            },
        )
    };
    if bump(&state, &format!("naw:relay-approve:{}", user.id)).await? > APPROVALS_PER_ACCOUNT {
        return refuse(StatusCode::TOO_MANY_REQUESTS, "error_busy");
    }
    let Some(code) = normalize(&form.code) else {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "error_code");
    };
    let Some((key, mut relay)) = find_by_code(&state, &code).await? else {
        return refuse(StatusCode::NOT_FOUND, "error_code");
    };
    if relay.approved_by.is_some() {
        return refuse(StatusCode::CONFLICT, "error_code");
    }
    let token = short_code();
    relay.approved_by = Some(user.id);
    relay.approver_name = Some(user.username.clone());
    relay.token_digest = Some(digest(&token));
    save(&state, &key, &relay, false).await?;
    forget(&state, &[code_key(&code)]).await;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user.id),
            action: "auth.relay_approve",
            entity_type: "user",
            entity_id: Some(user.id),
            meta: serde_json::json!({ "device": relay.device, "ip": relay.ip }),
        },
    )
    .await;
    render(
        &ctx,
        StatusCode::OK,
        View {
            mode: "token",
            token: Some(&token),
            next: "/",
            ..View::default()
        },
    )
}

#[derive(Deserialize)]
pub struct FinishForm {
    #[serde(default)]
    token: String,
}

/// POST /login/relay/finish: device A pastes the token and is signed in.
pub async fn finish(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<FinishForm>,
) -> Result<Response, AppError> {
    if !state.config.auth.enabled {
        return Ok(crate::errors::not_found());
    }
    let secure = secure_cookies(&state);
    let Some(secret) = relay_secret(&headers, secure) else {
        return Ok(Redirect::to("/login/relay?err=expired").into_response());
    };
    let key = relay_key(&secret);
    let Some(mut relay) = load(&state, &key).await? else {
        return Ok(Redirect::to("/login/relay?err=expired").into_response());
    };
    let (Some(user_id), Some(expected)) = (relay.approved_by, relay.token_digest.clone()) else {
        return Ok(Redirect::to("/login/relay?err=token").into_response());
    };
    let matches = normalize(&form.token)
        .map(|token| constant_eq(digest(&token).as_bytes(), expected.as_bytes()))
        .unwrap_or(false);
    if !matches {
        relay.tries += 1;
        if relay.tries >= TOKEN_TRIES {
            forget(&state, &[key]).await;
            return Ok(Redirect::to("/login/relay?err=expired").into_response());
        }
        save(&state, &key, &relay, false).await?;
        return Ok(Redirect::to("/login/relay?err=token").into_response());
    }
    forget(&state, &[key]).await;
    if session::install_banned(&state, user_id).await? {
        return Ok(Redirect::to("/login?err=generic").into_response());
    }
    let ip = crate::net::client_ip(&headers, peer, state.config.trust_proxy);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    let token = session::create(&state, user_id, Some(ip), user_agent).await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user_id),
            action: "auth.login",
            entity_type: "user",
            entity_id: Some(user_id),
            meta: serde_json::json!({ "provider": "relay" }),
        },
    )
    .await;
    let mut response = crate::auth::routes::sign_in_response(&state, &token, &relay.next);
    if let Some(cookie) = relay_cookie("", secure, 0) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_easy_to_type_and_normalize_back() {
        for _ in 0..50 {
            let code = short_code();
            assert_eq!(code.len(), 9);
            assert_eq!(normalize(&code).as_deref(), Some(code.as_str()));
            assert_eq!(
                normalize(&code.to_uppercase().replace('-', " ")).as_deref(),
                Some(code.as_str())
            );
        }
        for bad in ["", "abcd-efg", "abcd-efgh1", "0o1l-ilo0", "abcd efgh!"] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }

    #[test]
    fn digests_compare_in_constant_shape() {
        assert!(constant_eq(b"abc", b"abc"));
        assert!(!constant_eq(b"abc", b"abd"));
        assert!(!constant_eq(b"abc", b"ab"));
    }

    #[test]
    fn the_relay_cookie_is_host_prefixed_on_https() {
        let cookie = relay_cookie(&"a".repeat(43), true, 600).unwrap();
        let text = cookie.to_str().unwrap();
        assert!(
            text.starts_with("__Host-naw_relay=")
                && text.contains("Secure")
                && text.contains("HttpOnly")
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("__Host-naw_relay={}", "a".repeat(43))
                .parse()
                .unwrap(),
        );
        assert!(relay_secret(&headers, true).is_some());
        assert!(relay_secret(&headers, false).is_none());
    }
}
