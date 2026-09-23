//! Provider identity types shared by every backend.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Round 1 plus round 2 provider slugs. The registry keys on this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderId {
    Github,
    Discord,
    Telegram,
    Google,
    Yandex,
    Twitch,
    Steam,
    /// Loopback-only provider for end-to-end runs without secrets.
    Dev,
}

impl ProviderId {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Github => "github",
            ProviderId::Discord => "discord",
            ProviderId::Telegram => "telegram",
            ProviderId::Google => "google",
            ProviderId::Yandex => "yandex",
            ProviderId::Twitch => "twitch",
            ProviderId::Steam => "steam",
            ProviderId::Dev => "dev",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Some(match slug {
            "github" => ProviderId::Github,
            "discord" => ProviderId::Discord,
            "telegram" => ProviderId::Telegram,
            "google" => ProviderId::Google,
            "yandex" => ProviderId::Yandex,
            "twitch" => ProviderId::Twitch,
            "steam" => ProviderId::Steam,
            "dev" => ProviderId::Dev,
            _ => return None,
        })
    }
}

/// The normalized outcome of a provider round trip. Tokens never land here:
/// the backend drops them before returning.
#[derive(Clone, Debug)]
pub struct Identity {
    pub provider: ProviderId,
    pub provider_user_id: String,
    /// Present only when the provider certified the address.
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    /// Certified-email flag drives auto-link; Steam is always false.
    pub email_verified: bool,
    /// The provider handle the username policy starts from.
    pub handle: String,
    /// Trimmed profile for `oauth_identities.raw`. No tokens, no emails.
    pub raw: serde_json::Value,
}

/// Auth failures answer 4xx with a generic body, upstream trouble is 502,
/// only our own bugs stay 500. The log carries the detail, never the page.
#[derive(Debug)]
pub enum AuthError {
    /// Bad or missing state, unknown provider, malformed callback input.
    BadRequest(&'static str),
    /// The user bailed out at the provider or denied consent.
    Cancelled,
    /// State missing or already used, the flow must restart.
    StateExpired,
    /// The provider or the cache is unhappy. Generic page, log has detail.
    Upstream(String),
    /// The sign-in was fine, but it would create an account and registration
    /// is closed. Nothing was written.
    RegistrationClosed,
    /// The sign-in was fine, but the account is under an install-wide ban.
    Suspended,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AuthError::BadRequest(reason) => {
                tracing::warn!(reason, "auth bad request");
                (StatusCode::BAD_REQUEST, "authentication could not start")
            }
            AuthError::Cancelled => (StatusCode::SEE_OTHER, "login cancelled"),
            AuthError::StateExpired => {
                (StatusCode::BAD_REQUEST, "login session expired, start over")
            }
            AuthError::Upstream(detail) => {
                tracing::error!(detail = %detail, "auth upstream failure");
                (
                    StatusCode::BAD_GATEWAY,
                    "the identity provider is unavailable",
                )
            }
            AuthError::RegistrationClosed => (
                StatusCode::FORBIDDEN,
                "accounts on this wiki are created by its admins",
            ),
            AuthError::Suspended => (StatusCode::FORBIDDEN, "this account is suspended"),
        };
        if status == StatusCode::SEE_OTHER {
            return (
                status,
                [("Location", "/login?err=cancelled")],
                "login cancelled".to_string(),
            )
                .into_response();
        }
        (
            status,
            [("Cache-Control", "no-store")],
            format!(
                "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\" />\
                 <title>NotAnotherWiki</title></head><body><p>{message}</p>\
                 <p><a href=\"/login\">Back to sign in</a></p></body></html>"
            ),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_round_trip() {
        for slug in [
            "github", "discord", "telegram", "google", "yandex", "twitch", "steam", "dev",
        ] {
            let parsed = ProviderId::from_slug(slug).expect("known slug");
            assert_eq!(parsed.as_str(), slug);
        }
        assert!(ProviderId::from_slug("nope").is_none());
    }

    #[test]
    fn identity_holds_the_minimum() {
        let id = Identity {
            provider: ProviderId::Github,
            provider_user_id: "1234".to_string(),
            email: Some("user@example.test".to_string()),
            display_name: Some("User".to_string()),
            avatar_url: None,
            email_verified: true,
            handle: "user".to_string(),
            raw: serde_json::json!({"login": "user"}),
        };
        assert_eq!(id.provider.as_str(), "github");
        assert!(id.email_verified);
    }
}
