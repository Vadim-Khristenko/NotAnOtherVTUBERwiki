//! How a failed sign-in becomes a page.
//!
//! Handlers here return a status with an `auth_failed` marker and no body; the
//! error layer renders it in the reader's language through the active skin.
//! The sign-in page itself lives in `crate::account`.
//!
//! The wording rule from the hardening slice holds: the page says something
//! safe and generic, the log carries what actually failed.

use axum::http::StatusCode;
use axum::response::Response;

use super::types::AuthError;

/// Turns an `AuthError` into a response the error middleware renders as the
/// `auth_failed` page, in the reader's language and the active skin.
///
/// Each failure keeps its own wording through a variant: a cancelled flow is not
/// an outage, and telling somebody who pressed Cancel that a service is down
/// would be both wrong and alarming. The page says something safe; the log keeps
/// what actually failed.
///
/// A cancelled sign-in stays a 200, because nothing went wrong. The marker is
/// what makes it a page anyway.
pub fn auth_error(err: &AuthError) -> Response {
    use crate::errors::{Kind, MarkExt};
    let (status, variant) = match err {
        AuthError::BadRequest(reason) => {
            tracing::warn!(reason, "auth bad request");
            (StatusCode::BAD_REQUEST, "bad_request")
        }
        AuthError::Cancelled => (StatusCode::OK, "cancelled"),
        AuthError::StateExpired => (StatusCode::BAD_REQUEST, "expired"),
        AuthError::Upstream(detail) => {
            tracing::error!(detail = %detail, "auth upstream failure");
            (StatusCode::BAD_GATEWAY, "upstream")
        }
        // 403 and not 401: signing in again with the same provider changes
        // nothing, only an admin creating the account does.
        AuthError::RegistrationClosed => (StatusCode::FORBIDDEN, "closed"),
    };
    // No body: the reason shown to the reader comes from the language pack, and
    // an upstream detail is exactly the kind of text that must not reach the page.
    status.marked_as(Kind::AuthFailed, variant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::{Kind, Marked};

    fn marker(response: &Response) -> Option<Marked> {
        response.extensions().get::<Marked>().copied()
    }

    #[test]
    fn cancelling_is_a_page_and_not_a_failure() {
        // A reader who pressed Cancel did nothing wrong: no 4xx, but still a page,
        // which is what the marker is for.
        let response = auth_error(&AuthError::Cancelled);
        assert_eq!(response.status(), StatusCode::OK);
        let mark = marker(&response).expect("marked");
        assert_eq!(mark.kind, Kind::AuthFailed);
        assert_eq!(mark.variant, Some("cancelled"));
    }

    #[test]
    fn each_failure_keeps_its_own_wording_and_status() {
        let cases = [
            (
                AuthError::BadRequest("x"),
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
            (AuthError::StateExpired, StatusCode::BAD_REQUEST, "expired"),
            (
                AuthError::Upstream("down".to_string()),
                StatusCode::BAD_GATEWAY,
                "upstream",
            ),
        ];
        for (err, status, variant) in cases {
            let response = auth_error(&err);
            assert_eq!(response.status(), status, "{variant}");
            assert_eq!(marker(&response).and_then(|m| m.variant), Some(variant));
        }
    }

    #[tokio::test]
    async fn an_upstream_detail_never_reaches_the_body() {
        // Whatever a provider says about its own internals is for the log. The
        // response carries no body at all; the page text comes from the pack.
        let secretish = "token=abc123 leaked from provider";
        let response = auth_error(&AuthError::Upstream(secretish.to_string()));
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("body");
        assert!(body.is_empty(), "{body:?}");
    }
}
