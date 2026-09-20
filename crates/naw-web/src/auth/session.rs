//! Cookie sessions backed by the `sessions` table.
//!
//! One row per login, the cookie carries the session UUID, no signing:
//! the value is a random UUID and the server owns the lookup. Loading a
//! session joins users, expired rows delete themselves on sight.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, header};
use axum::middleware::Next;
use axum::response::Response;
use cookie::time::Duration;
use cookie::{Cookie, SameSite};
use uuid::Uuid;

use naw_core::state::AppState;

pub const SESSION_COOKIE: &str = "naw_session";

/// The signed-in user attached to every request as `Option<CurrentUser>`.
///
/// Only `id` has a reader so far. The rest is what the settings and profile
/// pages will render, and loading it here means those pages need no second
/// query.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CurrentUser {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub global_role: String,
}

/// Builds the session cookie attributes: Path=/, HttpOnly, SameSite=Lax,
/// Secure when the base URL is https, Max-Age from the session TTL.
fn build_cookie(value: &str, max_age_secs: i64, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::build((SESSION_COOKIE, value.to_owned()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(Duration::seconds(max_age_secs));
    if secure {
        cookie = cookie.secure(true);
    }
    cookie.build()
}

pub fn session_cookie(value: &str, ttl_hours: i64, secure: bool) -> String {
    build_cookie(value, ttl_hours * 3600, secure).to_string()
}

pub fn clear_cookie(secure: bool) -> String {
    let base = Cookie::build(SESSION_COOKIE)
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(Duration::ZERO)
        .build()
        .to_string();
    if secure {
        format!("{base}; Secure")
    } else {
        base
    }
}

/// Reads the session id from the Cookie header, if any.
pub fn session_id_from_headers(headers: &HeaderMap) -> Option<Uuid> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let mut parts = pair.trim().splitn(2, '=');
        if parts.next() == Some(SESSION_COOKIE) {
            let value = parts.next()?.trim();
            return value.parse::<Uuid>().ok();
        }
    }
    None
}

/// Loads the session joined with the user row. Expired rows delete
/// themselves on sight.
pub async fn load(state: &AppState, session_id: Uuid) -> Option<CurrentUser> {
    let row = sqlx::query!(
        r#"
        SELECT s.expires_at, u.id AS user_id, u.username,
               u.email AS email_opt,
               (u.email_verified_at IS NOT NULL) AS email_verified,
               u.global_role
        FROM sessions s
        JOIN users u ON u.id = s.user_id
        WHERE s.id = $1
        "#,
        session_id
    )
    .fetch_optional(&state.db)
    .await
    .ok()?;
    let row = row?;
    if row.expires_at <= chrono::Utc::now() {
        let _ = sqlx::query!("DELETE FROM sessions WHERE id = $1", session_id)
            .execute(&state.db)
            .await;
        return None;
    }
    Some(CurrentUser {
        id: row.user_id,
        username: row.username,
        email: row.email_opt,
        email_verified: row.email_verified.unwrap_or(false),
        global_role: row.global_role,
    })
}

pub async fn load_from_cookie(state: &AppState, headers: &HeaderMap) -> Option<CurrentUser> {
    let session_id = session_id_from_headers(headers)?;
    load(state, session_id).await
}

/// Inserts a fresh session row and returns its id.
pub async fn create(
    state: &AppState,
    user_id: Uuid,
    ip: Option<std::net::IpAddr>,
    user_agent: Option<&str>,
) -> Result<Uuid, naw_core::error::AppError> {
    let id = Uuid::new_v4();
    let expires = chrono::Duration::hours(state.config.auth.session_ttl_hours);
    sqlx::query!(
        "INSERT INTO sessions (id, user_id, expires_at, ip, user_agent) VALUES ($1, $2, $3, $4::inet, $5)",
        id,
        user_id,
        chrono::Utc::now() + expires,
        ip.map(ipnetwork::IpNetwork::from),
        user_agent
    )
    .execute(&state.db)
    .await?;
    Ok(id)
}

/// Deletes the session row.
pub async fn delete(state: &AppState, session_id: Uuid) {
    let _ = sqlx::query!("DELETE FROM sessions WHERE id = $1", session_id)
        .execute(&state.db)
        .await;
}

/// Inserts `Option<CurrentUser>` into the request extensions for every
/// route. Anonymous stays anonymous, handlers opt in explicitly.
pub async fn layer(State(app): State<AppState>, mut req: Request<Body>, next: Next) -> Response {
    let user = load_from_cookie(&app, req.headers()).await;
    req.extensions_mut().insert(user);
    let path = req.uri().path().to_string();
    let mut response = next.run(req).await;
    // Auth pages must never be cached with someone's chrome attached.
    if path == "/login"
        || path.starts_with("/settings")
        || path.starts_with("/auth")
        || path.starts_with("/verify-email")
    {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_attributes_match_the_locked_rules() {
        let cookie = session_cookie("0b6a3b6e-8f4a-4c1e-9d2a-000000000001", 720, false);
        assert!(cookie.starts_with("naw_session=0b6a3b6e"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Max-Age=2592000"));
        assert!(!cookie.contains("Secure"));
        let secure = session_cookie("x-value-is-ignored-anyway", 720, true);
        assert!(secure.contains("Secure"));
    }

    #[test]
    fn clear_cookie_zeroes_out() {
        let cookie = clear_cookie(false);
        assert!(cookie.starts_with("naw_session="));
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("HttpOnly"));
    }

    #[test]
    fn session_id_parses_from_a_browser_header() {
        let id = "6f9619ff-8b86-d011-b42d-00cf4fc964ff";
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            header::HeaderValue::from_str(&format!("other=1; naw_session={id}; theme=dark"))
                .expect("header"),
        );
        assert_eq!(session_id_from_headers(&headers), Some(id.parse().unwrap()));
        let mut broken = HeaderMap::new();
        broken.insert(
            header::COOKIE,
            header::HeaderValue::from_static("naw_session=not-a-uuid"),
        );
        assert_eq!(session_id_from_headers(&broken), None);
        let mut empty = HeaderMap::new();
        empty.insert(
            header::COOKIE,
            header::HeaderValue::from_static("theme=dark"),
        );
        assert_eq!(session_id_from_headers(&empty), None);
    }
}
