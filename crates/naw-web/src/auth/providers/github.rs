//! GitHub, plain OAuth2 with optional PKCE.
//!
//! Two quirks drive the shape of this file. The token endpoint answers
//! form-encoded unless asked for JSON, and the API returns 403 to any request
//! without a User-Agent. Both are handled once, in `http.rs`.
//!
//! The email needs its own call: when a user keeps their address private the
//! profile object carries `email: null`, and `/user/emails` is the only source.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use naw_core::config::OAuth2Creds;

use crate::auth::types::{AuthError, Identity, ProviderId};

use super::oauth2;
use super::{AuthorizeParams, CompleteParams, LoginProvider};

const AUTHORIZE: &str = "https://github.com/login/oauth/authorize";
const TOKEN: &str = "https://github.com/login/oauth/access_token";
const PROFILE: &str = "https://api.github.com/user";
const EMAILS: &str = "https://api.github.com/user/emails";
const SCOPE: &str = "read:user user:email";

/// GitHub versions its API through this header rather than the URL.
const API_VERSION: (&str, &str) = ("X-GitHub-Api-Version", "2022-11-28");
const ACCEPT: (&str, &str) = ("Accept", "application/vnd.github+json");

pub struct Github {
    client_id: String,
    client_secret: String,
}

impl Github {
    pub fn new(creds: &OAuth2Creds) -> Self {
        Self {
            client_id: creds.client_id.clone(),
            client_secret: creds.client_secret.clone(),
        }
    }
}

#[derive(Deserialize)]
struct Profile {
    id: i64,
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct Email {
    email: String,
    primary: bool,
    verified: bool,
}

/// Picks the address GitHub itself vouches for: primary and verified. A
/// verified non-primary address is accepted as a fallback, an unverified one
/// never is.
fn certified_email(mut entries: Vec<Email>) -> Option<String> {
    entries.retain(|entry| entry.verified && !entry.email.trim().is_empty());
    entries
        .iter()
        .find(|entry| entry.primary)
        .or_else(|| entries.first())
        .map(|entry| entry.email.trim().to_lowercase())
}

#[async_trait]
impl LoginProvider for Github {
    fn id(&self) -> ProviderId {
        ProviderId::Github
    }

    fn label(&self) -> &'static str {
        "GitHub"
    }

    fn authorize_url(&self, params: &AuthorizeParams<'_>) -> Result<String, AuthError> {
        oauth2::authorize_url(AUTHORIZE, &self.client_id, SCOPE, params, &[])
    }

    async fn complete(&self, params: &CompleteParams<'_>) -> Result<Identity, AuthError> {
        let code = params.code()?;
        let bearer = oauth2::exchange_code(
            params.http,
            &oauth2::Exchange {
                token_endpoint: TOKEN,
                client_id: &self.client_id,
                client_secret: &self.client_secret,
                code,
                redirect_uri: params.redirect_uri,
                code_verifier: params.code_verifier,
                // Without this the token endpoint answers form-encoded.
                accept_json: true,
                use_basic_auth: false,
            },
        )
        .await?
        .bearer()?;

        let headers = [ACCEPT, API_VERSION];
        let response = params.http.get(PROFILE, Some(&bearer), &headers).await?;
        if !response.is_success() {
            return Err(AuthError::Upstream(format!(
                "github profile returned {}",
                response.status
            )));
        }
        let profile: Profile = response.json()?;

        // A failed email call is not a failed login. GitHub can refuse the
        // scope, and an account without a usable address is still an account:
        // the user links one later in settings.
        let email = match params.http.get(EMAILS, Some(&bearer), &headers).await {
            Ok(response) if response.is_success() => {
                certified_email(response.json::<Vec<Email>>().unwrap_or_default())
            }
            Ok(response) => {
                tracing::warn!(status = response.status, "github emails call refused");
                None
            }
            Err(err) => {
                tracing::warn!(error = ?err, "github emails call failed");
                None
            }
        };

        Ok(Identity {
            provider: ProviderId::Github,
            provider_user_id: profile.id.to_string(),
            email_verified: email.is_some(),
            email,
            display_name: profile.name.filter(|name| !name.trim().is_empty()),
            avatar_url: profile.avatar_url,
            handle: profile.login.clone(),
            // Trimmed on purpose: no tokens, no scopes, no email. Enough to
            // debug a bad login and nothing more.
            raw: json!({ "id": profile.id, "login": profile.login }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::http::test_double::{Call, Scripted};

    fn provider() -> Github {
        Github::new(&OAuth2Creds {
            client_id: "Iv1.clientid".to_string(),
            client_secret: "the-secret".to_string(),
        })
    }

    fn query(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    const PROFILE_BODY: &str = r#"{"id":4242,"login":"FilianFan","name":"Filian Fan","avatar_url":"https://avatars.githubusercontent.com/u/4242"}"#;

    fn scripted(emails: Option<(u16, &str)>) -> Scripted {
        Scripted::new()
            .on("oauth/access_token", 200, r#"{"access_token":"gho_x"}"#)
            // The emails route must be registered before the profile route:
            // "api.github.com/user" is a prefix of "api.github.com/user/emails".
            .on(
                "api.github.com/user/emails",
                emails.map(|(status, _)| status).unwrap_or(403),
                emails.map(|(_, body)| body).unwrap_or("[]"),
            )
            .on("api.github.com/user", 200, PROFILE_BODY)
    }

    #[tokio::test]
    async fn a_verified_primary_email_is_taken_and_lowercased() {
        let http = scripted(Some((
            200,
            r#"[{"email":"Other@Example.test","primary":false,"verified":true},
                {"email":"Fan@Example.test","primary":true,"verified":true}]"#,
        )));
        let q = query(&[("code", "the-code")]);
        let identity = provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/github/callback",
                code_verifier: Some("verifier"),
                nonce: None,
                cache: None,
                http: &http,
            })
            .await
            .expect("login completes");

        assert_eq!(identity.provider, ProviderId::Github);
        assert_eq!(identity.provider_user_id, "4242");
        assert_eq!(identity.handle, "FilianFan");
        assert_eq!(identity.display_name.as_deref(), Some("Filian Fan"));
        assert_eq!(identity.email.as_deref(), Some("fan@example.test"));
        assert!(identity.email_verified);

        // The stored profile must never carry the token or the address.
        let raw = identity.raw.to_string();
        assert!(!raw.contains("gho_x"));
        assert!(!raw.to_lowercase().contains("example.test"));
    }

    #[tokio::test]
    async fn an_unverified_address_is_never_accepted() {
        let http = scripted(Some((
            200,
            r#"[{"email":"unverified@example.test","primary":true,"verified":false}]"#,
        )));
        let q = query(&[("code", "c")]);
        let identity = provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/github/callback",
                code_verifier: None,
                nonce: None,
                cache: None,
                http: &http,
            })
            .await
            .expect("login still completes");

        // No address rather than an unverified one: store.rs treats a present
        // email as certified, so handing it one here would defeat that.
        assert_eq!(identity.email, None);
        assert!(!identity.email_verified);
    }

    #[tokio::test]
    async fn a_refused_email_call_does_not_fail_the_login() {
        let http = scripted(None);
        let q = query(&[("code", "c")]);
        let identity = provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/github/callback",
                code_verifier: None,
                nonce: None,
                cache: None,
                http: &http,
            })
            .await
            .expect("login completes without an email");
        assert_eq!(identity.email, None);
        assert_eq!(identity.handle, "FilianFan");
    }

    #[tokio::test]
    async fn every_api_call_carries_the_headers_github_demands() {
        let http = scripted(Some((200, "[]")));
        let q = query(&[("code", "c")]);
        provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/github/callback",
                code_verifier: None,
                nonce: None,
                cache: None,
                http: &http,
            })
            .await
            .expect("completes");

        let calls = http.calls();
        let gets: Vec<_> = calls
            .iter()
            .filter_map(|call| match call {
                Call::Get {
                    url,
                    bearer,
                    headers,
                } => Some((url, bearer, headers)),
                _ => None,
            })
            .collect();
        assert_eq!(gets.len(), 2, "profile and emails");
        for (url, bearer, headers) in gets {
            assert_eq!(bearer.as_deref(), Some("gho_x"), "{url}");
            assert!(
                headers.iter().any(|(k, _)| k == API_VERSION.0),
                "{url} is missing the api version header"
            );
        }
    }

    #[tokio::test]
    async fn cancelling_at_github_is_not_an_error_page() {
        let http = scripted(None);
        let q = query(&[("error", "access_denied")]);
        let err = provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/github/callback",
                code_verifier: None,
                nonce: None,
                cache: None,
                http: &http,
            })
            .await
            .expect_err("cancel is not a login");
        assert!(matches!(err, AuthError::Cancelled));
        assert!(http.calls().is_empty(), "must not call the token endpoint");
    }

    #[test]
    fn the_authorize_url_asks_for_the_scopes_the_email_call_needs() {
        let url = provider()
            .authorize_url(&AuthorizeParams {
                redirect_uri: "https://snackers.wiki/auth/github/callback",
                state: "state",
                code_challenge: Some("challenge"),
                nonce: None,
            })
            .expect("builds");
        assert!(url.starts_with(AUTHORIZE));
        assert!(url.contains("scope=read%3Auser+user%3Aemail"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(!url.contains("the-secret"), "secret must never be in a URL");
    }

    #[test]
    fn email_selection_prefers_primary_then_any_verified() {
        let pick = |entries: Vec<(&str, bool, bool)>| {
            certified_email(
                entries
                    .into_iter()
                    .map(|(email, primary, verified)| Email {
                        email: email.to_string(),
                        primary,
                        verified,
                    })
                    .collect(),
            )
        };

        assert_eq!(
            pick(vec![("a@x.test", false, true), ("b@x.test", true, true)]),
            Some("b@x.test".to_string())
        );
        // Primary but unverified loses to a verified secondary.
        assert_eq!(
            pick(vec![("a@x.test", true, false), ("b@x.test", false, true)]),
            Some("b@x.test".to_string())
        );
        assert_eq!(pick(vec![("a@x.test", true, false)]), None);
        assert_eq!(pick(vec![]), None);
        assert_eq!(pick(vec![("   ", true, true)]), None);
    }
}
