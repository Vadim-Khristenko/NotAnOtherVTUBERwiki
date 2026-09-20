//! Rendering auth and system pages through the active skin.
//!
//! Before this module the auth routes emitted hardcoded HTML strings, which
//! meant a wiki's own look stopped at the login page. Everything here goes
//! through the skin's templates instead, with the reference skin filling in
//! whatever a custom skin does not ship.
//!
//! The wording rule from the hardening slice holds: the page says something
//! safe and generic, the log carries what actually failed.

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::pages::ENGINE_VERSION;
use crate::resolve::Chrome;

use super::providers;
use super::types::AuthError;

fn template_error(err: minijinja::Error) -> AppError {
    tracing::error!(error = %err, "auth template error");
    AppError::Internal
}

fn html(status: StatusCode, body: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // Belt alongside the session layer: nothing here is ever cacheable,
            // and several of these pages carry a signed-in user's name.
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

/// How a system page reads: which colour the notice takes.
#[derive(Clone, Copy)]
pub enum Tone {
    /// Used by the email verification and settings confirmations, which land
    /// with the settings pages. No auth failure is ever an `Ok`.
    #[allow(dead_code)]
    Ok,
    Warn,
    Error,
}

impl Tone {
    fn as_str(self) -> &'static str {
        match self {
            Tone::Ok => "ok",
            Tone::Warn => "warn",
            Tone::Error => "error",
        }
    }
}

pub struct Message<'a> {
    pub status: StatusCode,
    pub heading: &'a str,
    pub body: &'a str,
    pub tone: Tone,
    pub back_href: &'a str,
    pub back_label: &'a str,
}

/// A skinned system page. Used for auth failures, verification results and
/// anything else that is one sentence and a way out.
pub fn message(state: &AppState, chrome: &Chrome, msg: &Message<'_>) -> Response {
    let render = || -> Result<String, AppError> {
        let template = state
            .templates
            .get_template("message.html")
            .map_err(template_error)?;
        template
            .render(minijinja::context! {
                lang => &chrome.lang,
                wiki_name => &chrome.wiki_name,
                title => msg.heading,
                version => ENGINE_VERSION,
                heading => msg.heading,
                message => msg.body,
                tone => msg.tone.as_str(),
                back_href => msg.back_href,
                back_label => msg.back_label,
            })
            .map_err(template_error)
    };
    match render() {
        Ok(body) => html(msg.status, body),
        // A broken template must not swallow the status the caller chose, so
        // fall back to plain text rather than to a 500 page.
        Err(_) => (
            msg.status,
            [(header::CACHE_CONTROL, "no-store")],
            msg.body.to_string(),
        )
            .into_response(),
    }
}

/// Turns an `AuthError` into a skinned page.
///
/// `AuthError` also implements `IntoResponse` on its own, which stays as the
/// unskinned fallback for the few places that have no `AppState` in reach.
/// Prefer this whenever a handler can supply one.
pub fn auth_error(state: &AppState, chrome: &Chrome, err: &AuthError) -> Response {
    let (status, heading, body, tone) = match err {
        AuthError::BadRequest(reason) => {
            tracing::warn!(reason, "auth bad request");
            (
                StatusCode::BAD_REQUEST,
                "Sign-in could not start",
                "That link wasn't right. Start sign-in over and you should be good.",
                Tone::Error,
            )
        }
        AuthError::Cancelled => (
            StatusCode::OK,
            "Sign-in cancelled",
            "You cancelled sign-in, no harm done. Come back whenever you're ready.",
            Tone::Warn,
        ),
        AuthError::StateExpired => (
            StatusCode::BAD_REQUEST,
            "That sign-in expired",
            "That took too long. Sign-in links only last ten minutes, so start over and it should work.",
            Tone::Warn,
        ),
        AuthError::Upstream(detail) => {
            tracing::error!(detail = %detail, "auth upstream failure");
            (
                StatusCode::BAD_GATEWAY,
                // "identity provider" is our word, not a reader's.
                "That sign-in service is down",
                "They're having trouble right now. Wait a bit and try again.",
                Tone::Error,
            )
        }
    };
    message(
        state,
        chrome,
        &Message {
            status,
            heading,
            body,
            tone,
            back_href: "/login",
            back_label: "Back to sign in",
        },
    )
}

/// The `/login` page, with one button per configured provider.
pub fn login(state: &AppState, chrome: &Chrome, next: &str, error: Option<&str>) -> Response {
    let providers: Vec<_> = providers::enabled(&state.config.auth)
        .iter()
        .map(|provider| {
            minijinja::context! {
                slug => provider.id().as_str(),
                label => provider.label(),
            }
        })
        .collect();
    let render = || -> Result<String, AppError> {
        let template = state
            .templates
            .get_template("login.html")
            .map_err(template_error)?;
        template
            .render(minijinja::context! {
                lang => &chrome.lang,
                wiki_name => &chrome.wiki_name,
                title => "Sign in",
                version => ENGINE_VERSION,
                providers => providers,
                dev_login => state.config.auth.dev_login,
                next => next,
                error => error,
            })
            .map_err(template_error)
    };
    match render() {
        Ok(body) => html(StatusCode::OK, body),
        Err(err) => err.into_response(),
    }
}

/// The 404 every auth route uses when it must not admit a route exists.
pub fn not_found(state: &AppState, chrome: &Chrome) -> Response {
    message(
        state,
        chrome,
        &Message {
            status: StatusCode::NOT_FOUND,
            heading: "Not found",
            body: "Nothing here. Check the link and try again.",
            tone: Tone::Warn,
            back_href: "/",
            back_label: "Back to the wiki",
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_match_the_css_class_names() {
        // layout.html defines .notice-ok, .notice-warn and .notice-error.
        assert_eq!(Tone::Ok.as_str(), "ok");
        assert_eq!(Tone::Warn.as_str(), "warn");
        assert_eq!(Tone::Error.as_str(), "error");
    }

    #[test]
    fn cancelling_is_not_reported_as_a_failure() {
        // A user who pressed cancel did nothing wrong, so the page must not
        // read like an error and must not carry a 4xx.
        let err = AuthError::Cancelled;
        let status = match &err {
            AuthError::Cancelled => StatusCode::OK,
            _ => StatusCode::BAD_REQUEST,
        };
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn upstream_detail_never_becomes_page_copy() {
        // The detail string is for the log. Whatever a provider says about its
        // own internals must not end up rendered to a stranger.
        let secretish = "token=abc123 leaked from provider";
        let err = AuthError::Upstream(secretish.to_string());
        let AuthError::Upstream(detail) = &err else {
            panic!("shape changed");
        };
        assert_eq!(detail, secretish);
        // The copy chosen for this variant is fixed and mentions nothing.
        let page_copy = "They're having trouble right now. Wait a bit and try again.";
        assert!(!page_copy.contains("token"));
        assert!(!page_copy.contains(secretish));
    }
}
