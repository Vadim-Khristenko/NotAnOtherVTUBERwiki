//! Cookie sessions backed by the `sessions` table.
//!
//! One row per login, the cookie carries the session UUID, no signing:
//! the value is a random UUID and the server owns the lookup. Loading a
//! session joins users, expired rows delete themselves on sight.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
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
    /// What people read instead of the username, when set. Already cleaned by
    /// `display_name::clean`; templates still render it inside `<bdi>`.
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub email_verified: bool,
    pub global_role: String,
    /// Preferred interface language. Outranks `Accept-Language`, because
    /// somebody who picked a language in their settings means it, and their
    /// browser may well be somebody else's browser.
    pub locale: String,
    /// The password was issued by an admin and has not been replaced yet.
    /// Until it is, the session reaches only the change password page.
    pub must_change_password: bool,
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
               u.global_role, u.locale, u.must_change_password, u.display_name
        FROM sessions s
        JOIN users u ON u.id = s.user_id
        WHERE s.id = $1
          -- An install-wide ban ends the account's sessions where they stand.
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
        email: row.email_opt,
        email_verified: row.email_verified.unwrap_or(false),
        global_role: row.global_role,
        locale: row.locale,
        must_change_password: row.must_change_password,
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

/// Ends every session of `user_id` except `keep`. A password change or an
/// admin reset signs out every other device that might hold the old one.
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

/// Inserts `Option<CurrentUser>` into the request extensions for every
/// route. Anonymous stays anonymous, handlers opt in explicitly.
pub async fn layer(State(app): State<AppState>, mut req: Request<Body>, next: Next) -> Response {
    let user = load_from_cookie(&app, req.headers()).await;
    let path = req.uri().path().to_string();
    // An admin-issued password is a key handed over in plain text. Until its
    // owner replaces it, the session is good for replacing it and nothing else.
    if user.as_ref().is_some_and(|u| u.must_change_password) && !reachable_before_change(&path) {
        let target = password_page_for(req.uri());
        return axum::response::Redirect::to(&target).into_response();
    }
    req.extensions_mut().insert(user);
    let mut response = next.run(req).await;
    // Auth pages must never be cached with someone's chrome attached.
    if path.starts_with("/login")
        || path.starts_with("/account")
        || path.starts_with("/settings")
        || path.starts_with("/auth")
        || path.starts_with("/verify-email")
        // The admin panel lists accounts and audit rows. Nothing about it
        // belongs in a disk cache or a back-button restore.
        || path.starts_with("/admin")
    {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
    }
    response
}

/// Paths a session with a temporary password may still use: the page that
/// replaces it, signing out, the language switch and the things every page
/// loads on its own (icons, the manifest, health checks).
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

/// The change password page, remembering where the person was headed so the
/// change lands them there. Only a local path is kept.
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
