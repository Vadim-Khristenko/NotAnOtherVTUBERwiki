//! Cookie sessions in the `sessions` table.
//!
//! The cookie carries a random 256-bit token; the table keeps only a digest
//! of it (as the row id), so a database dump or backup holds no usable
//! session. On https the cookie is `__Host-` prefixed, which a browser only
//! accepts from this host, with no Domain, so a sibling subdomain cannot
//! plant one. Expired rows are deleted on sight.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use cookie::time::Duration;
use cookie::{Cookie, SameSite};
use uuid::Uuid;

use naw_core::state::AppState;

/// The cookie name on plain http, for local development.
pub const SESSION_COOKIE: &str = "naw_session";
/// The cookie name on https.
pub const SECURE_SESSION_COOKIE: &str = "__Host-naw_session";

fn cookie_name(secure: bool) -> &'static str {
    if secure {
        SECURE_SESSION_COOKIE
    } else {
        SESSION_COOKIE
    }
}

/// The row id of a session token: the first 16 bytes of its SHA-256.
pub fn id_of(token: &str) -> Uuid {
    let digest = crate::auth::token_hash(token);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

/// Whether a cookie value has the shape of a token this engine issues.
fn looks_like_token(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The signed-in user, attached to every request as `Option<CurrentUser>`.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CurrentUser {
    pub id: Uuid,
    pub username: String,
    /// Already cleaned by `display_name::clean`; rendered inside `<bdi>`.
    pub display_name: Option<String>,
    /// The avatar's address under `/media`.
    pub avatar_url: Option<String>,
    pub email: Option<String>,
    pub email_verified: bool,
    pub global_role: String,
    /// Preferred interface language; outranks `Accept-Language`.
    pub locale: String,
    /// An admin-issued password not replaced yet; only the change page is open.
    pub must_change_password: bool,
}

/// Path=/, HttpOnly, SameSite=Lax, Secure on https, Max-Age from the TTL.
fn build_cookie(value: &str, max_age_secs: i64, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::build((cookie_name(secure), value.to_owned()))
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
    let base = Cookie::build(cookie_name(secure))
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

/// The session row id for the token in the Cookie header, if any.
pub fn session_id_from_headers(headers: &HeaderMap, secure: bool) -> Option<Uuid> {
    let name = cookie_name(secure);
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, value)| *key == name && looks_like_token(value.trim()))
        .map(|(_, value)| id_of(value.trim()))
}

/// Loads the session with its user; an expired row is deleted.
pub async fn load(state: &AppState, session_id: Uuid) -> Option<CurrentUser> {
    let row = sqlx::query!(
        r#"
        SELECT s.expires_at, u.id AS user_id, u.username,
               u.email AS email_opt,
               (u.email_verified_at IS NOT NULL) AS email_verified,
               u.global_role, u.locale, u.must_change_password, u.display_name,
               u.avatar_key
        FROM sessions s
        JOIN users u ON u.id = s.user_id
        WHERE s.id = $1
          -- An install-wide ban ends the sessions where they stand.
          AND NOT EXISTS (
            SELECT 1 FROM sanctions b
            WHERE b.user_id = u.id AND b.wiki_id IS NULL AND b.kind = 'ban'
              AND b.lifted_at IS NULL AND (b.expires_at IS NULL OR b.expires_at > now()))
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
        display_name: row.display_name,
        avatar_url: row.avatar_key.as_deref().map(crate::media::url_for_key),
        email: row.email_opt,
        email_verified: row.email_verified.unwrap_or(false),
        global_role: row.global_role,
        locale: row.locale,
        must_change_password: row.must_change_password,
    })
}

pub async fn load_from_cookie(state: &AppState, headers: &HeaderMap) -> Option<CurrentUser> {
    let session_id = session_id_from_headers(headers, crate::auth::routes::secure_cookies(state))?;
    load(state, session_id).await
}

/// Inserts a fresh session row and returns its token, the cookie value.
pub async fn create(
    state: &AppState,
    user_id: Uuid,
    ip: Option<std::net::IpAddr>,
    user_agent: Option<&str>,
) -> Result<String, naw_core::error::AppError> {
    let token = crate::auth::random_token();
    let id = id_of(&token);
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
    Ok(token)
}

/// Ends every session of `user_id` except `keep`.
pub async fn delete_others(
    state: &AppState,
    user_id: Uuid,
    keep: Option<Uuid>,
) -> Result<u64, naw_core::error::AppError> {
    let result = sqlx::query!(
        "DELETE FROM sessions WHERE user_id = $1 AND ($2::uuid IS NULL OR id <> $2)",
        user_id,
        keep
    )
    .execute(&state.db)
    .await?;
    Ok(result.rows_affected())
}

/// Whether an install-wide ban keeps this account out right now.
pub async fn install_banned(
    state: &AppState,
    user_id: Uuid,
) -> Result<bool, naw_core::error::AppError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM sanctions
             WHERE user_id = $1 AND wiki_id IS NULL AND kind = 'ban' AND lifted_at IS NULL
               AND (expires_at IS NULL OR expires_at > now())) AS "banned!""#,
        user_id
    )
    .fetch_one(&state.db)
    .await?)
}

/// Deletes the session row.
pub async fn delete(state: &AppState, session_id: Uuid) {
    let _ = sqlx::query!("DELETE FROM sessions WHERE id = $1", session_id)
        .execute(&state.db)
        .await;
}

/// Inserts `Option<CurrentUser>` into every request's extensions.
pub async fn layer(State(app): State<AppState>, mut req: Request<Body>, next: Next) -> Response {
    let user = load_from_cookie(&app, req.headers()).await;
    let path = req.uri().path().to_string();
    // Until an admin-issued password is replaced, the session can only replace it.
    if user.as_ref().is_some_and(|u| u.must_change_password) && !reachable_before_change(&path) {
        let target = password_page_for(req.uri());
        return axum::response::Redirect::to(&target).into_response();
    }
    req.extensions_mut().insert(user);
    let mut response = next.run(req).await;
    // Auth pages must never be cached with someone's chrome.
    if path.starts_with("/login")
        || path.starts_with("/account")
        || path.starts_with("/settings")
        || path.starts_with("/auth")
        || path.starts_with("/verify-email")
        || path.starts_with("/admin")
    {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
    }
    response
}

/// What a session with a temporary password may still reach.
fn reachable_before_change(path: &str) -> bool {
    matches!(
        path,
        PASSWORD_PAGE
            | "/logout"
            | "/lang"
            | "/health"
            | "/ready"
            | "/favicon.ico"
            | "/favicon-96x96.png"
            | "/apple-touch-icon.png"
            | "/site.webmanifest"
            | "/web-app-manifest-192x192.png"
            | "/web-app-manifest-512x512.png"
    )
}

/// Where a password change happens.
pub const PASSWORD_PAGE: &str = "/settings/password";

/// The change password page, keeping a local destination.
fn password_page_for(uri: &axum::http::Uri) -> String {
    let wanted = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let next = super::redirect::safe_next(Some(wanted));
    if next == "/" {
        PASSWORD_PAGE.to_string()
    } else {
        format!(
            "{PASSWORD_PAGE}?next={}",
            super::redirect::encode_component(&next)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temporary_password_reaches_only_the_way_out() {
        assert!(reachable_before_change("/settings/password"));
        assert!(reachable_before_change("/logout"));
        assert!(!reachable_before_change("/"));
        assert!(!reachable_before_change("/admin"));
        assert!(!reachable_before_change("/settings/password/extra"));
    }

    #[test]
    fn the_detour_remembers_a_local_destination_only() {
        let uri: axum::http::Uri = "/some-page/edit?x=1".parse().unwrap();
        assert_eq!(
            password_page_for(&uri),
            "/settings/password?next=%2Fsome-page%2Fedit%3Fx%3D1"
        );
        let root: axum::http::Uri = "/".parse().unwrap();
        assert_eq!(password_page_for(&root), "/settings/password");
    }

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
    fn https_cookies_are_host_prefixed() {
        let secure = session_cookie("token", 720, true);
        assert!(secure.starts_with("__Host-naw_session=token"), "{secure}");
        assert!(secure.contains("Secure") && secure.contains("Path=/"));
        assert!(!secure.contains("Domain"), "{secure}");
        assert!(clear_cookie(true).starts_with("__Host-naw_session="));
    }

    fn cookie_header(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            header::HeaderValue::from_str(value).expect("header"),
        );
        headers
    }

    #[test]
    fn the_row_id_is_a_digest_of_the_token() {
        let token = crate::auth::random_token();
        let headers = cookie_header(&format!("other=1; naw_session={token}; theme=dark"));
        assert_eq!(
            session_id_from_headers(&headers, false),
            Some(id_of(&token))
        );
        // The id is not the token and does not contain it.
        assert!(!id_of(&token).to_string().contains(&token[..8]));
        assert_eq!(id_of(&token), id_of(&token));
        assert_ne!(id_of(&token), id_of(&crate::auth::random_token()));
    }

    #[test]
    fn only_the_cookie_for_the_scheme_counts() {
        let token = crate::auth::random_token();
        // On https a plain-named cookie, which any sibling subdomain could
        // set, is ignored.
        let plain = cookie_header(&format!("naw_session={token}"));
        assert_eq!(session_id_from_headers(&plain, true), None);
        let host = cookie_header(&format!("__Host-naw_session={token}"));
        assert_eq!(session_id_from_headers(&host, true), Some(id_of(&token)));
    }

    #[test]
    fn values_that_are_not_tokens_are_ignored() {
        for bad in [
            "naw_session=6f9619ff-8b86-d011-b42d-00cf4fc964ff",
            "naw_session=not-a-token",
            "naw_session=",
            "theme=dark",
        ] {
            assert_eq!(
                session_id_from_headers(&cookie_header(bad), false),
                None,
                "{bad}"
            );
        }
    }
}
