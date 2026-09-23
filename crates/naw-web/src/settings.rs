//! The signed-in person's own settings at `/settings`. Every change acts on
//! the asking account only and is a POST.

use axum::extract::{Extension, Form, Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::providers;
use crate::auth::redirect::encode_component;
use crate::auth::session::{self, CurrentUser};
use crate::pages::{ENGINE_VERSION, template_error};

const AVATAR_CHANGES_PER_HOUR: i64 = 10;

/// Sessions listed before the list is cut.
const SESSIONS_SHOWN: i64 = 20;

fn sign_in_first() -> Response {
    Redirect::to(&format!("/login?next={}", encode_component("/settings"))).into_response()
}

/// A short "browser on platform" label for a user agent.
fn device_label(user_agent: Option<&str>) -> String {
    let ua = user_agent.unwrap_or_default();
    let browser = [
        ("Edg/", "Edge"),
        ("OPR/", "Opera"),
        ("YaBrowser/", "Yandex Browser"),
        ("Firefox/", "Firefox"),
        ("Chrome/", "Chrome"),
        ("Safari/", "Safari"),
    ]
    .iter()
    .find(|(needle, _)| ua.contains(needle))
    .map(|(_, name)| *name);
    let platform = [
        ("Android", "Android"),
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Windows", "Windows"),
        ("Mac OS X", "macOS"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(needle, _)| ua.contains(needle))
    .map(|(_, name)| *name);
    match (browser, platform) {
        (Some(b), Some(p)) => format!("{b}, {p}"),
        (Some(b), None) => b.to_string(),
        (None, Some(p)) => p.to_string(),
        (None, None) => String::new(),
    }
}

#[derive(Deserialize)]
pub struct SettingsQuery {
    /// Which change just landed.
    #[serde(default)]
    saved: Option<String>,
    /// Why the last change was refused, as a message key.
    #[serde(default)]
    error: Option<String>,
}

/// GET /settings
pub async fn page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<SettingsQuery>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let Some(ctx) = crate::resolve::context(&state, &headers, Some(&user)).await? else {
        return Ok(crate::errors::not_found());
    };

    let account = sqlx::query!(
        r#"SELECT created_at, (password_hash IS NOT NULL) AS "has_password!",
                  password_changed_at
           FROM users WHERE id = $1"#,
        user.id
    )
    .fetch_one(&state.db)
    .await?;

    let identities = sqlx::query!(
        "SELECT provider, display_name, created_at, last_login_at
         FROM oauth_identities WHERE user_id = $1 ORDER BY created_at",
        user.id
    )
    .fetch_all(&state.db)
    .await?;
    let linked: Vec<String> = identities.iter().map(|row| row.provider.clone()).collect();
    let identity_rows: Vec<minijinja::Value> = identities
        .into_iter()
        .map(|row| {
            minijinja::context! {
                provider => row.provider,
                name => row.display_name,
                since => row.created_at.format("%Y-%m-%d").to_string(),
                last => row.last_login_at.map(|t| t.format("%Y-%m-%d").to_string()),
            }
        })
        .collect();
    let linkable: Vec<minijinja::Value> = providers::enabled(&state.config.auth)
        .iter()
        .filter(|provider| !linked.iter().any(|p| p == provider.id().as_str()))
        .map(|provider| {
            minijinja::context! {
                slug => provider.id().as_str(),
                label => provider.label(),
            }
        })
        .collect();

    let current_session = session::session_id_from_headers(&headers);
    let sessions = sqlx::query!(
        "SELECT id, created_at, ip::text AS ip, user_agent
         FROM sessions WHERE user_id = $1 AND expires_at > now()
         ORDER BY created_at DESC LIMIT $2",
        user.id,
        SESSIONS_SHOWN
    )
    .fetch_all(&state.db)
    .await?;
    let session_rows: Vec<minijinja::Value> = sessions
        .into_iter()
        .map(|row| {
            minijinja::context! {
                device => device_label(row.user_agent.as_deref()),
                ip => row.ip.map(|ip| ip.trim_end_matches("/32").trim_end_matches("/128").to_string()),
                since => row.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                current => Some(row.id) == current_session,
            }
        })
        .collect();

    let policy = crate::policy::accounts(&state).await?;

    // The saved preference, or "" to follow the browser.
    let chosen = if ctx.skin.messages.has(&user.locale) {
        user.locale.clone()
    } else {
        String::new()
    };

    let template = ctx
        .skin
        .env
        .get_template("settings.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("settings.title"),
                version => ENGINE_VERSION,
                joined => account.created_at.format("%Y-%m-%d").to_string(),
                email => user.email.clone(),
                email_verified => user.email_verified,
                has_password => account.has_password,
                password_changed => account.password_changed_at.map(|t| t.format("%Y-%m-%d").to_string()),
                chosen_locale => chosen,
                my_display_name => user.display_name.clone(),
                display_name_max => crate::display_name::MAX_LEN,
                avatar_max_mb => naw_core::html::mib(state.config.avatar_max_bytes),
                identities => identity_rows,
                linkable => linkable,
                sessions => session_rows,
                saved => query.saved.as_deref().filter(|s| matches!(*s, "language" | "sessions" | "username" | "display_name" | "avatar" | "avatar_removed")),
                error => crate::pages::message_key(query.error.as_deref(), &["rename_", "display_name_", "avatar_"]),
                rename_enabled => policy.rename_enabled,
                rename_cooldown => policy.rename_cooldown_days,
                alias_days => policy.alias_days,
                aliases_disabled => policy.aliases_disabled,
            }
        })
        .map_err(template_error)?;
    Ok(crate::pages::private_page(StatusCode::OK, html))
}

#[derive(Deserialize)]
pub struct LanguageForm {
    /// A language code, or empty to follow the browser.
    #[serde(default)]
    locale: String,
}

/// POST /settings/language
pub async fn set_language(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Form(form): Form<LanguageForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let wanted = form.locale.trim();
    // Anything the wiki cannot show means "follow the browser".
    let locale = if state.skin.current().messages.has(wanted) {
        wanted.to_string()
    } else {
        String::new()
    };
    sqlx::query!(
        "UPDATE users SET locale = $2 WHERE id = $1",
        user.id,
        locale
    )
    .execute(&state.db)
    .await?;
    // A stale `?lang=` cookie would outrank the new account setting.
    let mut response = Redirect::to("/settings?saved=language").into_response();
    if let Ok(value) = header::HeaderValue::from_str(&crate::lang::clear_cookie()) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    Ok(response)
}

/// POST /settings/sessions/end-others
pub async fn end_other_sessions(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let keep = session::session_id_from_headers(&headers);
    let ended = session::delete_others(&state, user.id, keep).await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user.id),
            action: "auth.sessions_end_others",
            entity_type: "user",
            entity_id: Some(user.id),
            meta: serde_json::json!({ "ended": ended }),
        },
    )
    .await;
    Ok(Redirect::to("/settings?saved=sessions").into_response())
}

#[derive(Deserialize)]
pub struct DisplayNameForm {
    #[serde(default)]
    display_name: String,
}

/// POST /settings/display-name
pub async fn set_display_name(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Form(form): Form<DisplayNameForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let cleaned = match crate::display_name::clean(&form.display_name) {
        Ok(cleaned) => cleaned,
        Err(problem) => {
            return Ok(
                Redirect::to(&format!("/settings?error={}#s-display", problem.key()))
                    .into_response(),
            );
        }
    };
    sqlx::query!(
        "UPDATE users SET display_name = $2 WHERE id = $1",
        user.id,
        cleaned
    )
    .execute(&state.db)
    .await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user.id),
            action: "user.display_name",
            entity_type: "user",
            entity_id: Some(user.id),
            meta: serde_json::json!({ "from": user.display_name, "to": cleaned }),
        },
    )
    .await;
    Ok(Redirect::to("/settings?saved=display_name").into_response())
}

/// POST /settings/avatar: one image under the avatar limit. Old files stay
/// in storage, where another account may share them by hash.
pub async fn set_avatar(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let max = state.config.avatar_max_bytes;
    let refused = |refusal: crate::media::Refusal| {
        Redirect::to(&format!(
            "/settings?error=avatar_{}#s-avatar",
            refusal.slug()
        ))
        .into_response()
    };
    let recent = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM audit_log
           WHERE user_id = $1 AND action = 'user.avatar' AND created_at > now() - interval '1 hour'"#,
        user.id
    )
    .fetch_one(&state.db)
    .await?;
    if recent >= AVATAR_CHANGES_PER_HOUR {
        return Ok(refused(crate::media::Refusal::Quota));
    }
    let data = match crate::media::read_file_field(&mut multipart, "avatar", max).await {
        Ok(Some((_, data))) => data,
        Ok(None) => return Ok(refused(crate::media::Refusal::Empty)),
        Err(refusal) => return Ok(refused(refusal)),
    };
    let stored = match crate::media::store(&state, "avatars", data, max).await? {
        Ok(stored) => stored,
        Err(refusal) => return Ok(refused(refusal)),
    };
    sqlx::query!(
        "UPDATE users SET avatar_key = $2 WHERE id = $1",
        user.id,
        stored.key
    )
    .execute(&state.db)
    .await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user.id),
            action: "user.avatar",
            entity_type: "user",
            entity_id: Some(user.id),
            meta: serde_json::json!({ "key": stored.key, "size": stored.size }),
        },
    )
    .await;
    Ok(Redirect::to("/settings?saved=avatar").into_response())
}

/// POST /settings/avatar/remove
pub async fn remove_avatar(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    sqlx::query!("UPDATE users SET avatar_key = NULL WHERE id = $1", user.id)
        .execute(&state.db)
        .await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user.id),
            action: "user.avatar_removed",
            entity_type: "user",
            entity_id: Some(user.id),
            meta: serde_json::json!({}),
        },
    )
    .await;
    Ok(Redirect::to("/settings?saved=avatar_removed").into_response())
}

#[derive(Deserialize)]
pub struct UsernameForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    current: String,
}

/// Why a rename is refused under `policy::accounts`, as a `settings.` key.
async fn rename_refusal(
    state: &AppState,
    policy: &naw_core::config::AccountPolicy,
    user: &CurrentUser,
    wanted: &str,
    current_password: &str,
) -> Result<Option<&'static str>, AppError> {
    if !policy.rename_enabled {
        return Ok(Some("rename_disabled"));
    }
    if wanted == user.username {
        return Ok(Some("rename_same"));
    }
    if !crate::auth::username::is_valid(wanted) {
        return Ok(Some("rename_invalid"));
    }
    if state
        .config
        .auth
        .reserved_usernames
        .iter()
        .any(|name| name.eq_ignore_ascii_case(wanted))
    {
        return Ok(Some("rename_reserved"));
    }
    let row = sqlx::query!(
        "SELECT password_hash, username_changed_at FROM users WHERE id = $1",
        user.id
    )
    .fetch_one(&state.db)
    .await?;
    if let Some(last) = row.username_changed_at
        && last > chrono::Utc::now() - chrono::Duration::days(policy.rename_cooldown_days)
    {
        return Ok(Some("rename_too_soon"));
    }
    // The password, when set, proves the owner is at the keyboard.
    if row.password_hash.is_some()
        && !crate::auth::password::verify(current_password.to_string(), row.password_hash).await
    {
        return Ok(Some("rename_password"));
    }
    // Taken by an account, or reserved as someone else's former name; an
    // expired alias is free, and your own former names are yours.
    let taken = sqlx::query!(
        "SELECT 1 AS one FROM users WHERE lower(username) = $1 AND id <> $2
         UNION ALL
         SELECT 1 AS one FROM user_aliases
         WHERE lower(alias) = $1 AND user_id <> $2
           AND NOT $4
           AND ($3::bigint = 0 OR created_at > now() - make_interval(days => $3::int))",
        wanted,
        user.id,
        policy.alias_days,
        policy.aliases_disabled
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();
    Ok(taken.then_some("rename_taken"))
}

/// POST /settings/username
pub async fn change_username(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Form(form): Form<UsernameForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let policy = crate::policy::accounts(&state).await?;
    let wanted = form.username.trim().to_lowercase();
    if let Some(key) = rename_refusal(&state, &policy, &user, &wanted, &form.current).await? {
        return Ok(Redirect::to(&format!("/settings?error={key}#s-username")).into_response());
    }

    let mut tx = state.db.begin().await?;
    // An expired reservation of the wanted name gives way.
    sqlx::query!("DELETE FROM user_aliases WHERE lower(alias) = $1", wanted)
        .execute(&mut *tx)
        .await?;
    // The old name stays reserved as an alias, so nobody can pass for this
    // account and old profile links keep working.
    if !policy.aliases_disabled && policy.max_aliases > 0 {
        sqlx::query!(
            "INSERT INTO user_aliases (alias, user_id) VALUES ($1, $2)
             ON CONFLICT (alias) DO UPDATE SET created_at = now(), user_id = EXCLUDED.user_id",
            user.username,
            user.id
        )
        .execute(&mut *tx)
        .await?;
    }
    // Expired aliases first, then everything past the limit, oldest first.
    sqlx::query!(
        "DELETE FROM user_aliases
         WHERE user_id = $1
           AND (($2::bigint > 0 AND created_at <= now() - make_interval(days => $2::int))
                OR alias NOT IN (SELECT alias FROM user_aliases WHERE user_id = $1
                                 ORDER BY created_at DESC LIMIT $3))",
        user.id,
        policy.alias_days,
        policy.max_aliases
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE users SET username = $2, username_changed_at = now() WHERE id = $1",
        user.id,
        wanted
    )
    .execute(&mut *tx)
    .await?;
    // The profile page is addressed by name.
    sqlx::query!(
        "UPDATE pages SET slug = $2, title = $2, updated_at = now()
         WHERE namespace = 'user' AND slug = $1",
        user.username,
        wanted
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user.id),
            action: "user.rename",
            entity_type: "user",
            entity_id: Some(user.id),
            meta: serde_json::json!({ "from": user.username, "to": wanted }),
        },
    )
    .await;
    Ok(Redirect::to("/settings?saved=username").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_get_short_names() {
        let chrome_win = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36";
        assert_eq!(device_label(Some(chrome_win)), "Chrome, Windows");
        let safari_iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/19.0 Mobile/15E148 Safari/604.1";
        assert_eq!(device_label(Some(safari_iphone)), "Safari, iPhone");
        let edge =
            "Mozilla/5.0 (Windows NT 10.0) AppleWebKit/537.36 Chrome/150 Safari/537.36 Edg/150";
        assert_eq!(device_label(Some(edge)), "Edge, Windows");
        assert_eq!(device_label(None), "");
    }
}
