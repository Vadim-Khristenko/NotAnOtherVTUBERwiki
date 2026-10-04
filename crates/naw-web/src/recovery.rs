//! Getting back into an account after forgetting its password.
//!
//! `/password/forgot` takes a username (or an address on the account) and,
//! when the account has a linked Telegram chat, the bot sends it a reset
//! link. The answer reads the same whether or not the account exists, and
//! the sending happens off the response path, so the page tells a stranger
//! nothing. The bot's `/reset` does the same from the chat.
//!
//! A link works once, for [`RESET_MINUTES`]; asking again ends the earlier
//! one. Only the SHA-256 of its token is stored. `/password/reset` sets the
//! new password, ends every session of the account and tells the owner in
//! Telegram. Requests are counted per name and per address in Valkey.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Extension, Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::password;
use crate::auth::session::CurrentUser;
use crate::pages::{ENGINE_VERSION, private_page, template_error};
use crate::resolve::Ctx;

/// Minutes a reset link works.
pub(crate) const RESET_MINUTES: i64 = 30;

/// Reset requests one name may make in an hour; the rest are dropped quietly.
const PER_NAME: i64 = 3;

/// Reset requests one address may make in an hour, across names.
const PER_ADDRESS: i64 = 10;

/// Whether a request under `key` is within `max` an hour. Counts it. With
/// Valkey down, requests go through, as sign-in does.
pub(crate) async fn allow(state: &AppState, key: &str, max: i64) -> bool {
    let Ok(mut conn) = state.valkey.get().await else {
        return true;
    };
    let counted: Result<(i64,), _> = deadpool_redis::redis::pipe()
        .cmd("INCR")
        .arg(key)
        .cmd("EXPIRE")
        .arg(key)
        .arg(3600)
        .arg("NX")
        .ignore()
        .query_async(&mut conn)
        .await;
    match counted {
        Ok((n,)) => n <= max,
        Err(err) => {
            tracing::error!(error = %err, "reset throttle failed");
            true
        }
    }
}

/// A new reset token for `user_id`; earlier unused ones stop working.
pub(crate) async fn issue(
    db: &sqlx::PgPool,
    user_id: Uuid,
    channel: &str,
) -> Result<String, sqlx::Error> {
    let token = crate::auth::random_token();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "UPDATE password_resets SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO password_resets (id, user_id, token_hash, channel, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(mins => $5))",
        Uuid::new_v4(),
        user_id,
        &crate::auth::token_hash(&token)[..],
        channel,
        RESET_MINUTES as i32
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(token)
}

/// The page a reset link opens.
pub(crate) fn reset_url(origin: &str, token: &str) -> String {
    format!("{origin}/password/reset?token={token}")
}

/// Where a reset link may point: the wiki's own domain, else the configured
/// base URL. Never the Host header, which the person asking chooses: a link
/// carrying the victim's token to the asker's site would hand them the
/// account. `None` when neither is set.
fn trusted_origin(state: &AppState, ctx: &Ctx) -> Option<String> {
    link_origin(
        ctx.wiki.domain.as_deref(),
        state.config.auth.base_url.as_deref(),
    )
}

fn link_origin(domain: Option<&str>, base_url: Option<&str>) -> Option<String> {
    if let Some(domain) = domain.map(str::trim).filter(|d| !d.is_empty()) {
        return Some(format!("https://{}", domain.to_lowercase()));
    }
    base_url
        .map(|base| base.trim().trim_end_matches('/').to_string())
        .filter(|base| base.starts_with("https://") || base.starts_with("http://"))
}

/// The account a token still opens: its id and username.
async fn holder(db: &sqlx::PgPool, token: &str) -> Result<Option<(Uuid, String)>, AppError> {
    if token.is_empty() || token.len() > 100 {
        return Ok(None);
    }
    Ok(sqlx::query!(
        "SELECT u.id, u.username FROM password_resets r JOIN users u ON u.id = r.user_id
         WHERE r.token_hash = $1 AND r.used_at IS NULL AND r.expires_at > now()",
        &crate::auth::token_hash(token)[..]
    )
    .fetch_optional(db)
    .await?
    .map(|row| (row.id, row.username)))
}

/// Uses the token and sets the password in one go: every session of the
/// account ends, and every other open link with it. `None` when the token
/// stopped working in between.
async fn redeem(
    db: &sqlx::PgPool,
    token: &str,
    hash: &str,
) -> Result<Option<(Uuid, u64)>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let Some(user_id) = sqlx::query_scalar!(
        "UPDATE password_resets SET used_at = now()
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
         RETURNING user_id",
        &crate::auth::token_hash(token)[..]
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    sqlx::query!(
        "UPDATE users SET password_hash = $2, must_change_password = false, password_changed_at = now()
         WHERE id = $1",
        user_id,
        hash
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE password_resets SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    let ended = sqlx::query!("DELETE FROM sessions WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(Some((user_id, ended)))
}

/// A reset asked for, in the account's audit trail.
pub(crate) async fn requested(state: &AppState, user_id: Uuid, via: &str) {
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: None,
            action: "auth.password_reset_requested",
            entity_type: "user",
            entity_id: Some(user_id),
            meta: serde_json::json!({ "via": via }),
        },
    )
    .await;
}

/// Whether this install can send a reset link at all.
pub(crate) fn available() -> bool {
    crate::telegram::bot().is_some()
}

/// What the recovery template shows.
enum View<'a> {
    /// The form asking for the account.
    Ask { error: Option<&'a str> },
    /// The answer, the same for every name.
    Sent,
    /// The new password form for a working link.
    Choose {
        token: &'a str,
        username: &'a str,
        error: Option<&'a str>,
    },
    /// A link that is used, expired or unknown.
    Dead,
    /// The password is set.
    Done,
}

fn render(ctx: &Ctx, status: StatusCode, view: View<'_>) -> Result<Response, AppError> {
    let (mode, token, username, error) = match view {
        View::Ask { error } => ("ask", "", "", error),
        View::Sent => ("sent", "", "", None),
        View::Choose {
            token,
            username,
            error,
        } => ("choose", token, username, error),
        View::Dead => ("dead", "", "", None),
        View::Done => ("done", "", "", None),
    };
    let html = ctx
        .skin
        .env
        .get_template("recovery.html")
        .map_err(template_error)?
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("recovery.title"),
                version => ENGINE_VERSION,
                mode => mode,
                available => available(),
                bot => crate::telegram::bot().map(|b| b.username().to_string()),
                token => token,
                account => username,
                minutes => RESET_MINUTES,
                min_len => password::MIN_LEN,
                error => error.map(|key| ctx.t(key)),
            }
        })
        .map_err(template_error)?;
    let mut response = private_page(status, html);
    // The token is in the address: it must not travel on in a Referer.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}

async fn signed_out_ctx(
    state: &AppState,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
) -> Result<Result<Ctx, Response>, AppError> {
    if !state.config.auth.enabled || !state.config.auth.password_login {
        return Ok(Err(crate::errors::not_found()));
    }
    crate::resolve::required(state, headers, user).await
}

/// GET /password/forgot
pub async fn forgot_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(signed_out_ctx(&state, &headers, user.as_ref()).await?);
    render(&ctx, StatusCode::OK, View::Ask { error: None })
}

#[derive(Deserialize)]
pub struct ForgotForm {
    #[serde(default)]
    username: String,
}

/// POST /password/forgot
pub async fn forgot_send(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ForgotForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(signed_out_ctx(&state, &headers, user.as_ref()).await?);
    let name: String = form
        .username
        .trim()
        .to_lowercase()
        .chars()
        .take(254)
        .collect();
    if name.is_empty() {
        return render(
            &ctx,
            StatusCode::BAD_REQUEST,
            View::Ask {
                error: Some("recovery.error_missing"),
            },
        );
    }
    if !available() {
        return render(
            &ctx,
            StatusCode::SERVICE_UNAVAILABLE,
            View::Ask { error: None },
        );
    }
    let ip = crate::net::client_ip(&headers, peer, state.config.trust_proxy);
    if !allow(&state, &format!("naw:reset:ip:{ip}"), PER_ADDRESS).await {
        return render(
            &ctx,
            StatusCode::TOO_MANY_REQUESTS,
            View::Ask {
                error: Some("recovery.error_throttled"),
            },
        );
    }
    let account = sqlx::query!(
        r#"SELECT u.id, u.username, (tl.chat_id IS NOT NULL) AS "linked!"
           FROM users u LEFT JOIN telegram_links tl ON tl.user_id = u.id
           WHERE lower(u.username) = $1 OR lower(u.email) = $1
              OR u.id = (SELECT a.user_id FROM user_aliases a WHERE a.alias = $1)
           LIMIT 1"#,
        name
    )
    .fetch_optional(&state.db)
    .await?;
    // Counted for every name, found or not, so the time taken says nothing.
    let within = allow(&state, &format!("naw:reset:name:{name}"), PER_NAME).await;
    // Past this point every name gets the same page; the work, if any, goes on
    // its own task.
    if let Some(account) = account.filter(|a| a.linked)
        && let Some(origin) = trusted_origin(&state, &ctx)
        && within
    {
        let state = state.clone();
        let device = crate::settings::device_label(
            headers
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok()),
        );
        tokio::spawn(async move {
            send_link(&state, account.id, &account.username, &origin, &device).await;
        });
    }
    render(&ctx, StatusCode::OK, View::Sent)
}

/// Issues a link and sends it to the account's chat.
async fn send_link(state: &AppState, user_id: Uuid, username: &str, origin: &str, device: &str) {
    if crate::auth::session::install_banned(state, user_id)
        .await
        .unwrap_or(true)
    {
        return;
    }
    let token = match issue(&state.db, user_id, "telegram").await {
        Ok(token) => token,
        Err(err) => {
            tracing::error!(error = %err, "could not issue a reset link");
            return;
        }
    };
    requested(state, user_id, "web").await;
    let lang = crate::telegram::language_of(state, user_id).await;
    let (wiki, _) = crate::telegram::site(state).await;
    let url = reset_url(origin, &token);
    let device = if device.is_empty() {
        crate::telegram::text(state, &lang, "device_unknown", &[])
    } else {
        device.to_string()
    };
    let body = crate::telegram::text(
        state,
        &lang,
        "reset_link_web",
        &[
            ("wiki", &wiki),
            ("user", username),
            ("url", &url),
            ("device", &device),
            ("minutes", &RESET_MINUTES.to_string()),
        ],
    );
    let label = crate::telegram::text(state, &lang, "reset_button", &[]);
    crate::telegram::send_to_user(
        state,
        user_id,
        "password_reset",
        &body,
        Some((&label, &url)),
    )
    .await;
}

#[derive(Deserialize)]
pub struct ResetQuery {
    #[serde(default)]
    token: String,
}

/// GET /password/reset?token=
pub async fn reset_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Query(query): Query<ResetQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(signed_out_ctx(&state, &headers, user.as_ref()).await?);
    let token = query.token.trim();
    match holder(&state.db, token).await? {
        Some((_, username)) => render(
            &ctx,
            StatusCode::OK,
            View::Choose {
                token,
                username: &username,
                error: None,
            },
        ),
        None => render(&ctx, StatusCode::GONE, View::Dead),
    }
}

#[derive(Deserialize)]
pub struct ResetForm {
    #[serde(default)]
    token: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    confirm: String,
}

/// POST /password/reset
pub async fn reset_save(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<ResetForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(signed_out_ctx(&state, &headers, user.as_ref()).await?);
    let token = form.token.trim();
    let Some((_, username)) = holder(&state.db, token).await? else {
        return render(&ctx, StatusCode::GONE, View::Dead);
    };
    // Checked before the token is used, so a typo does not cost the link.
    if let Err(problem) = password::check_new(&form.password, &form.confirm, &username, None) {
        let key = format!("account.{}", problem.key());
        return render(
            &ctx,
            StatusCode::BAD_REQUEST,
            View::Choose {
                token,
                username: &username,
                error: Some(&key),
            },
        );
    }
    let hash = password::hash(form.password).await?;
    let Some((user_id, ended)) = redeem(&state.db, token, &hash).await? else {
        return render(&ctx, StatusCode::GONE, View::Dead);
    };
    crate::auth::throttle::clear_account(&state.valkey, &username).await;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user_id),
            action: "auth.password_reset",
            entity_type: "user",
            entity_id: Some(user_id),
            meta: serde_json::json!({ "sessions_ended": ended }),
        },
    )
    .await;
    crate::telegram::alert(&state, user_id, crate::telegram::Alert::PasswordReset);
    render(&ctx, StatusCode::OK, View::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reset_link_never_points_where_the_asker_says() {
        assert_eq!(
            link_origin(Some("Alpha.Filian.Wiki"), Some("https://other.example")).as_deref(),
            Some("https://alpha.filian.wiki")
        );
        assert_eq!(
            link_origin(None, Some("https://wiki.example/")).as_deref(),
            Some("https://wiki.example")
        );
        assert_eq!(link_origin(None, None), None, "no Host header fallback");
        assert_eq!(link_origin(Some(" "), Some("javascript:alert(1)")), None);
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::PgPool;

    async fn user(db: &PgPool, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash, must_change_password)
             VALUES ($1, $2, $2 || '@example.test', 'old', true)",
        )
        .bind(id)
        .bind(name)
        .execute(db)
        .await
        .expect("user");
        id
    }

    async fn session(db: &PgPool, user_id: Uuid) {
        sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, now() + interval '1 day')")
            .bind(Uuid::new_v4())
            .bind(user_id)
            .execute(db)
            .await
            .expect("session");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_link_works_once_and_ends_every_session(db: PgPool) {
        let alice = user(&db, "alice").await;
        session(&db, alice).await;
        session(&db, alice).await;
        let token = issue(&db, alice, "telegram").await.expect("issue");
        assert_eq!(
            holder(&db, &token).await.expect("q").map(|(id, _)| id),
            Some(alice)
        );
        assert_eq!(holder(&db, "not-a-token").await.expect("q"), None);

        let (who, ended) = redeem(&db, &token, "new-hash")
            .await
            .expect("redeem")
            .expect("works");
        assert_eq!((who, ended), (alice, 2));
        let (hash, must): (Option<String>, bool) =
            sqlx::query_as("SELECT password_hash, must_change_password FROM users WHERE id = $1")
                .bind(alice)
                .fetch_one(&db)
                .await
                .expect("user");
        assert_eq!(hash.as_deref(), Some("new-hash"));
        assert!(!must, "a reset password is the owner's own, not temporary");
        assert!(
            redeem(&db, &token, "again").await.expect("q").is_none(),
            "once"
        );
        assert_eq!(holder(&db, &token).await.expect("q"), None);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_newer_link_ends_the_older_and_an_old_one_expires(db: PgPool) {
        let alice = user(&db, "alice").await;
        let first = issue(&db, alice, "telegram").await.expect("issue");
        let second = issue(&db, alice, "telegram").await.expect("issue");
        assert_eq!(holder(&db, &first).await.expect("q"), None, "replaced");
        assert!(holder(&db, &second).await.expect("q").is_some());
        sqlx::query("UPDATE password_resets SET expires_at = now() - interval '1 minute'")
            .execute(&db)
            .await
            .expect("age");
        assert_eq!(holder(&db, &second).await.expect("q"), None, "expired");
        assert!(redeem(&db, &second, "x").await.expect("q").is_none());
    }
}
