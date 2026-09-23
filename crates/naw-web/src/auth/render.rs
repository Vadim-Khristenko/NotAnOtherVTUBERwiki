//! How a failed sign-in becomes a page: a status with an `auth_failed`
//! marker and no body, rendered by the error layer.

use axum::http::StatusCode;
use axum::response::Response;

use super::types::AuthError;

/// An `AuthError` as an `auth_failed` page with its own variant wording. A
/// cancelled sign-in stays a 200; the marker still makes it a page.
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
        // Not 401: signing in again changes nothing, only an admin can help.
        AuthError::RegistrationClosed => (StatusCode::FORBIDDEN, "closed"),
        AuthError::Suspended => (StatusCode::FORBIDDEN, "suspended"),
    };
    // No body: upstream detail must never reach the page.
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
        let secretish = "token=abc123 leaked from provider";
        let response = auth_error(&AuthError::Upstream(secretish.to_string()));
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("body");
        assert!(body.is_empty(), "{body:?}");
    }
}
