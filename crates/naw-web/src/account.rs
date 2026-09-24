//! Username and password sign-in, and changing the password.
//!
//! An admin creates an account with a temporary password; the session layer
//! keeps such a session on the change page until the owner replaces it.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Extension, Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use naw_core::config::Registration;
use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::password;
use crate::auth::redirect::{encode_component, safe_next};
use crate::auth::session::{self, CurrentUser, PASSWORD_PAGE};
use crate::auth::{providers, routes, throttle};
use crate::pages::{ENGINE_VERSION, template_error};
use crate::resolve::Ctx;

/// The login form.
#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    next: String,
}

/// The change password form.
#[derive(Deserialize)]
pub struct PasswordForm {
    #[serde(default)]
    current: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    confirm: String,
    #[serde(default)]
    next: String,
}

use crate::pages::private_page;

async fn context(
    state: &AppState,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
) -> Result<Option<Ctx>, AppError> {
    crate::resolve::context(state, headers, user).await
}

/// The login page beyond the chrome.
struct LoginView<'a> {
    next: &'a str,
    /// Key under `account.` for the message at the top.
    error: Option<&'a str>,
    username: &'a str,
}

fn render_login(
    state: &AppState,
    ctx: &Ctx,
    status: StatusCode,
    view: &LoginView<'_>,
) -> Result<Response, AppError> {
    let auth = &state.config.auth;
    let providers: Vec<_> = providers::enabled(auth)
        .iter()
        .map(|provider| {
            minijinja::context! {
                slug => provider.id().as_str(),
                label => provider.label(),
            }
        })
        .collect();
    let template = ctx
        .skin
        .env
        .get_template("login.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("account.sign_in"),
                version => ENGINE_VERSION,
                providers => providers,
                password_login => auth.password_login,
                registration_closed => auth.registration == Registration::Closed,
                apply_url => auth.apply_url.as_deref(),
                dev_login => auth.dev_login,
                next => view.next,
                next_encoded => encode_component(view.next),
                error => view.error.map(|key| ctx.t(&format!("account.{key}"))),
                form_username => view.username,
            }
        })
        .map_err(template_error)?;
    Ok(private_page(status, html))
}

/// GET /login: the password form when enabled, then one button per provider.
pub async fn login_page(
    State(state): State<AppState>,
    Query(params): Query<BTreeMap<String, String>>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !state.config.auth.enabled {
        return Ok(crate::errors::not_found());
    }
    let next = safe_next(params.get("next").map(String::as_str));
    if user.is_some() {
        return Ok(Redirect::to(&next).into_response());
    }
    let Some(ctx) = context(&state, &headers, None).await? else {
        return Ok(crate::errors::not_found());
    };
    // `?err=` selects a fixed message; it is never echoed.
    let error = params.get("err").map(|err| match err.as_str() {
        "cancelled" => "error_cancelled",
        "expired" => "error_expired",
        _ => "error_generic",
    });
    render_login(
        &state,
        &ctx,
        StatusCode::OK,
        &LoginView {
            next: &next,
            error,
            username: "",
        },
    )
}

/// POST /login/password
pub async fn password_login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let auth = &state.config.auth;
    if !auth.enabled || !auth.password_login {
        return Ok(crate::errors::not_found());
    }
    let Some(ctx) = context(&state, &headers, None).await? else {
        return Ok(crate::errors::not_found());
    };
    let next = safe_next(Some(form.next.as_str()));
    let username = form.username.trim().to_lowercase();
    let ip = crate::net::client_ip(&headers, peer, state.config.trust_proxy);
    let again = |status, error| {
        render_login(
            &state,
            &ctx,
            status,
            &LoginView {
                next: &next,
                error: Some(error),
                username: &username,
            },
        )
    };

    if username.is_empty() || form.password.is_empty() {
        return again(StatusCode::BAD_REQUEST, "error_missing");
    }
    if form.password.chars().count() > password::MAX_LEN {
        return again(StatusCode::UNAUTHORIZED, "error_password");
    }
    if throttle::is_blocked(&state.valkey, &username, ip).await {
        tracing::warn!(%ip, "sign-in throttled");
        return again(StatusCode::TOO_MANY_REQUESTS, "error_throttled");
    }

    let row = sqlx::query!(
        "SELECT id, username, password_hash, must_change_password
         FROM users WHERE lower(username) = $1",
        username
    )
    .fetch_optional(&state.db)
    .await?;
    let stored = row.as_ref().and_then(|row| row.password_hash.clone());
    // Always one full verification, so an unknown user and a wrong password
    // look and take the same.
    let verified = password::verify(form.password, stored).await;
    let target = row.as_ref().map(|row| row.id);
    let Some(row) = row.filter(|_| verified) else {
        throttle::record_miss(&state.valkey, &username, ip).await;
        // Only against an account that exists: a guess at a name nobody has
        // would let anyone fill the log with invented names.
        if let Some(target) = target {
            sign_in_refused(&state, target, "wrong_password").await;
        }
        return again(StatusCode::UNAUTHORIZED, "error_password");
    };
    throttle::clear_account(&state.valkey, &username).await;
    // The password was right, so saying the account is suspended reveals nothing.
    if session::install_banned(&state, row.id).await? {
        sign_in_refused(&state, row.id, "suspended").await;
        return again(StatusCode::FORBIDDEN, "error_suspended");
    }

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let session_id = session::create(&state, row.id, Some(ip), user_agent).await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(row.id),
            action: "auth.login",
            entity_type: "user",
            entity_id: Some(row.id),
            meta: serde_json::json!({ "provider": "password" }),
        },
    )
    .await;
    tracing::info!(username = %row.username, "signed in with a password");

    let target = if row.must_change_password {
        change_target(&next)
    } else {
        next
    };
    Ok(routes::sign_in_response(&state, &session_id, &target))
}

/// A refused password sign-in, for the account's owner and the admins to see.
/// No address and nothing typed is kept. Written off the response path, so a
/// refusal for an account that exists takes no longer than for one that does not.
async fn sign_in_refused(state: &AppState, user_id: uuid::Uuid, why: &'static str) {
    let db = state.db.clone();
    tokio::spawn(async move {
        crate::audit::record_or_log(
            &db,
            crate::audit::Entry {
                wiki_id: None,
                user_id: None,
                action: "auth.login_refused",
                entity_type: "user",
                entity_id: Some(user_id),
                meta: serde_json::json!({ "provider": "password", "why": why }),
            },
        )
        .await;
    });
}

/// The change password page, carrying the destination along.
fn change_target(next: &str) -> String {
    if next == "/" {
        PASSWORD_PAGE.to_string()
    } else {
        format!("{PASSWORD_PAGE}?next={}", encode_component(next))
    }
}

fn render_password_form(
    ctx: &Ctx,
    status: StatusCode,
    user: &CurrentUser,
    needs_current: bool,
    next: &str,
    error: Option<&str>,
) -> Result<Response, AppError> {
    let template = ctx
        .skin
        .env
        .get_template("password.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("account.password_title"),
                version => ENGINE_VERSION,
                forced => user.must_change_password,
                needs_current => needs_current,
                min_len => password::MIN_LEN,
                next => next,
                error => error.map(|key| ctx.t(&format!("account.{key}"))),
            }
        })
        .map_err(template_error)?;
    Ok(private_page(status, html))
}

/// Whether to ask for the current password: not for a temporary one just
/// typed, nor for an account that never had a password.
async fn needs_current(
    state: &AppState,
    user: &CurrentUser,
) -> Result<(bool, Option<String>), AppError> {
    let stored = sqlx::query_scalar!("SELECT password_hash FROM users WHERE id = $1", user.id)
        .fetch_one(&state.db)
        .await?;
    Ok((!user.must_change_password && stored.is_some(), stored))
}

/// GET /settings/password
pub async fn password_page(
    State(state): State<AppState>,
    Query(params): Query<BTreeMap<String, String>>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(
            Redirect::to(&format!("/login?next={}", encode_component(PASSWORD_PAGE)))
                .into_response(),
        );
    };
    let Some(ctx) = context(&state, &headers, Some(&user)).await? else {
        return Ok(crate::errors::not_found());
    };
    let next = safe_next(params.get("next").map(String::as_str));
    let (needs_current, _) = needs_current(&state, &user).await?;
    render_password_form(&ctx, StatusCode::OK, &user, needs_current, &next, None)
}

/// POST /settings/password
pub async fn change_password(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(Redirect::to("/login").into_response());
    };
    let Some(ctx) = context(&state, &headers, Some(&user)).await? else {
        return Ok(crate::errors::not_found());
    };
    let next = safe_next(Some(form.next.as_str()));
    let (needs_current, stored) = needs_current(&state, &user).await?;
    let refuse =
        |status, key| render_password_form(&ctx, status, &user, needs_current, &next, Some(key));

    if needs_current && !password::verify(form.current.clone(), stored.clone()).await {
        return refuse(StatusCode::UNAUTHORIZED, "current_wrong");
    }
    let previous = needs_current.then_some(form.current.as_str());
    if let Err(problem) =
        password::check_new(&form.password, &form.confirm, &user.username, previous)
    {
        return refuse(StatusCode::BAD_REQUEST, problem.key());
    }
    // A temporary password is only known as a hash here.
    if stored.is_some() && password::verify(form.password.clone(), stored).await {
        return refuse(StatusCode::BAD_REQUEST, password::Problem::Unchanged.key());
    }

    let hash = password::hash(form.password).await?;
    sqlx::query!(
        "UPDATE users SET password_hash = $2, must_change_password = false,
                password_changed_at = now()
         WHERE id = $1",
        user.id,
        hash
    )
    .execute(&state.db)
    .await?;
    // Sign out every other device; this one just proved who it is.
    let keep =
        session::session_id_from_headers(&headers, crate::auth::routes::secure_cookies(&state));
    let ended = session::delete_others(&state, user.id, keep).await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user.id),
            action: "auth.password_change",
            entity_type: "user",
            entity_id: Some(user.id),
            meta: serde_json::json!({ "was_temporary": user.must_change_password, "sessions_ended": ended }),
        },
    )
    .await;

    let template = ctx
        .skin
        .env
        .get_template("message.html")
        .map_err(template_error)?;
    let heading = ctx.t("account.password_changed");
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => heading.clone(),
                version => ENGINE_VERSION,
                heading => heading,
                tone => "ok",
                message => ctx.t("account.password_changed_body"),
                back_href => next,
                back_label => ctx.t("account.continue"),
            }
        })
        .map_err(template_error)?;
    Ok(private_page(StatusCode::OK, html))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_change_page_keeps_where_you_were_going() {
        assert_eq!(change_target("/"), "/settings/password");
        assert_eq!(
            change_target("/lore/edit"),
            "/settings/password?next=%2Flore%2Fedit"
        );
    }
}
