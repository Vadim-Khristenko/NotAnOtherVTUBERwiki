//! Telegram, a full OIDC authorization code flow with PKCE.
//!
//! Three things make it unlike GitHub and Discord:
//!
//! 1. There is no userinfo endpoint. Every claim comes out of the id_token, so
//!    the JWT verification in `super::super::jwks` is the whole identity path
//!    rather than a nicety.
//! 2. The client pair goes in an `Authorization: Basic` header, not the body.
//! 3. **There is no email, ever.** Telegram offers a phone number and nothing
//!    else, so a Telegram-only account reaches `store::finish_login` with no
//!    address and links one later in settings.
//!
//! `client_id` is the bot id from BotFather, and it doubles as the `aud` claim.
//!
//! Verified against the live discovery document on 2026-09-20:
//! `https://oauth.telegram.org/.well-known/openid-configuration`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use naw_core::config::OAuth2Creds;

use crate::auth::jwks;
use crate::auth::types::{AuthError, Identity, ProviderId};

use super::oauth2;
use super::{AuthorizeParams, CompleteParams, LoginProvider};

const ISSUER: &str = "https://oauth.telegram.org";
const AUTHORIZE: &str = "https://oauth.telegram.org/auth";
const TOKEN: &str = "https://oauth.telegram.org/token";
const JWKS_URI: &str = "https://oauth.telegram.org/.well-known/jwks.json";

/// `phone` is offered and deliberately not requested: a wiki login has no use
/// for a phone number, and asking for one is a worse consent screen.
/// `telegram:bot_access` is also on offer and is the likely path to letting the
/// bot DM a user for login confirmations; it stays out until that flow is built
/// and its semantics are confirmed.
const SCOPE: &str = "openid profile";

pub struct Telegram {
    client_id: String,
    client_secret: String,
}

impl Telegram {
    pub fn new(creds: &OAuth2Creds) -> Self {
        Self {
            client_id: creds.client_id.clone(),
            client_secret: creds.client_secret.clone(),
        }
    }
}

/// The id_token claims we read. `aud` and `exp` are checked by the verifier, so
/// they are absent here on purpose.
#[derive(Deserialize)]
struct Claims {
    sub: String,
    name: Option<String>,
    preferred_username: Option<String>,
    picture: Option<String>,
    /// OIDC allows a nonce claim; when the provider echoes ours it must match.
    nonce: Option<String>,
}

#[async_trait]
impl LoginProvider for Telegram {
    fn id(&self) -> ProviderId {
        ProviderId::Telegram
    }

    fn label(&self) -> &'static str {
        "Telegram"
    }

    fn authorize_url(&self, params: &AuthorizeParams<'_>) -> Result<String, AuthError> {
        oauth2::authorize_url(AUTHORIZE, &self.client_id, SCOPE, params, &[])
    }

    async fn complete(&self, params: &CompleteParams<'_>) -> Result<Identity, AuthError> {
        let code = params.code()?;
        let tokens = oauth2::exchange_code(
            params.http,
            &oauth2::Exchange {
                token_endpoint: TOKEN,
                client_id: &self.client_id,
                client_secret: &self.client_secret,
                code,
                redirect_uri: params.redirect_uri,
                code_verifier: params.code_verifier,
                accept_json: false,
                // Telegram wants base64(client_id:client_secret) in a header.
                use_basic_auth: true,
            },
        )
        .await?;

        // Surface a body-level error before complaining about a missing token.
        if let Some(error) = &tokens.error {
            let detail = tokens.error_description.clone().unwrap_or_default();
            return Err(AuthError::Upstream(format!(
                "telegram token endpoint refused: {error} {detail}"
            )));
        }
        let id_token = tokens
            .id_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                AuthError::Upstream(
                    "telegram returned no id_token, and it is the only identity source".to_string(),
                )
            })?;

        let claims: Claims = jwks::verify(
            id_token,
            &jwks::Expect {
                issuer: ISSUER,
                audience: &self.client_id,
                jwks_uri: JWKS_URI,
            },
            params.http,
            params.cache,
        )
        .await?;

        // A replayed id_token from another login attempt fails here.
        if let (Some(expected), Some(got)) = (params.nonce, claims.nonce.as_deref())
            && expected != got
        {
            return Err(AuthError::BadRequest("id_token nonce does not match"));
        }

        if claims.sub.trim().is_empty() {
            return Err(AuthError::Upstream(
                "telegram id_token carried no sub".to_string(),
            ));
        }

        let handle = claims
            .preferred_username
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("tg-{}", claims.sub));

        Ok(Identity {
            provider: ProviderId::Telegram,
            provider_user_id: claims.sub.clone(),
            // Telegram has no email to give. Not "unverified", absent.
            email: None,
            email_verified: false,
            display_name: claims.name.filter(|name| !name.trim().is_empty()),
            avatar_url: claims.picture,
            handle,
            raw: json!({ "sub": claims.sub, "preferred_username": claims.preferred_username }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::http::test_double::{Call, Scripted};

    fn provider() -> Telegram {
        Telegram::new(&OAuth2Creds {
            client_id: "8100000000".to_string(),
            client_secret: "botfather-secret".to_string(),
        })
    }

    fn query(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    async fn complete_with(token_body: &str) -> Result<Identity, AuthError> {
        let http = Scripted::new()
            .on("oauth.telegram.org/token", 200, token_body)
            .on("jwks.json", 200, r#"{"keys":[]}"#);
        let q = query(&[("code", "the-code")]);
        provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/telegram/callback",
                code_verifier: Some("verifier"),
                nonce: Some("the-nonce"),
                cache: None,
                http: &http,
            })
            .await
    }

    #[test]
    fn the_authorize_url_matches_the_live_discovery_document() {
        let url = provider()
            .authorize_url(&AuthorizeParams {
                redirect_uri: "https://snackers.wiki/auth/telegram/callback",
                state: "st",
                code_challenge: Some("ch"),
                nonce: Some("no"),
            })
            .expect("builds");
        assert!(url.starts_with("https://oauth.telegram.org/auth?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("nonce=no"));
        assert!(url.contains("scope=openid+profile"));
        assert!(
            !url.contains("phone"),
            "a wiki login has no business asking for a phone number"
        );
        assert!(!url.contains("botfather-secret"));
    }

    #[tokio::test]
    async fn the_token_call_uses_basic_auth_and_keeps_the_secret_out_of_the_body() {
        let http = Scripted::new()
            .on("oauth.telegram.org/token", 200, r#"{"access_token":"a"}"#)
            .on("jwks.json", 200, r#"{"keys":[]}"#);
        let q = query(&[("code", "c")]);
        let _ = provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/telegram/callback",
                code_verifier: Some("v"),
                nonce: None,
                cache: None,
                http: &http,
            })
            .await;

        assert!(
            http.posted(0, "client_secret").is_none(),
            "the secret belongs in the header only"
        );
        assert_eq!(http.posted(0, "code_verifier").as_deref(), Some("v"));
        match &http.calls()[0] {
            Call::PostForm {
                basic, accept_json, ..
            } => {
                assert_eq!(basic.as_deref(), Some("8100000000"));
                assert!(!accept_json, "telegram answers json without being asked");
            }
            other => panic!("expected a form post, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_id_token_fails_loudly_because_it_is_the_only_identity() {
        // An access_token alone is useless: Telegram has no userinfo endpoint,
        // so silently continuing would mean a login with no subject.
        let err = complete_with(r#"{"access_token":"only-this","token_type":"Bearer"}"#)
            .await
            .expect_err("no id_token means no identity");
        let AuthError::Upstream(detail) = err else {
            panic!("expected upstream, got {err:?}");
        };
        assert!(detail.contains("id_token"));
    }

    #[tokio::test]
    async fn an_empty_id_token_is_treated_as_missing() {
        let err = complete_with(r#"{"access_token":"a","id_token":""}"#)
            .await
            .expect_err("empty is not a token");
        assert!(matches!(err, AuthError::Upstream(_)));
    }

    #[tokio::test]
    async fn a_token_endpoint_error_is_reported_before_the_missing_id_token() {
        let err =
            complete_with(r#"{"error":"invalid_grant","error_description":"code already used"}"#)
                .await
                .expect_err("refused");
        let AuthError::Upstream(detail) = err else {
            panic!("expected upstream");
        };
        assert!(
            detail.contains("invalid_grant"),
            "the real cause must survive into the log: {detail}"
        );
    }

    #[tokio::test]
    async fn an_unverifiable_id_token_never_produces_an_identity() {
        // kid oidc-1, but the JWKS served here is empty, so verification must
        // fail rather than fall back to trusting the payload.
        let forged = "eyJhbGciOiJSUzI1NiIsImtpZCI6Im9pZGMtMSJ9.eyJpc3MiOiJodHRwczovL29hdXRoLnRlbGVncmFtLm9yZyIsImF1ZCI6IjgxMDAwMDAwMDAiLCJzdWIiOiI5OTkiLCJleHAiOjk5OTk5OTk5OTl9.forged";
        let body = format!(r#"{{"access_token":"a","id_token":"{forged}"}}"#);
        let err = complete_with(&body)
            .await
            .expect_err("an unsigned payload is not an identity");
        assert!(matches!(
            err,
            AuthError::Upstream(_) | AuthError::BadRequest(_)
        ));
    }

    #[tokio::test]
    async fn cancelling_at_telegram_skips_the_token_call() {
        let http = Scripted::new();
        let q = query(&[("error", "access_denied")]);
        let err = provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/telegram/callback",
                code_verifier: None,
                nonce: None,
                cache: None,
                http: &http,
            })
            .await
            .expect_err("cancel");
        assert!(matches!(err, AuthError::Cancelled));
        assert!(http.calls().is_empty());
    }
}
