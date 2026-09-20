//! The provider registry and the contract every login backend implements.
//!
//! A provider is a stateless holder of its own credentials, so the registry
//! builds one on demand from config rather than living in `AppState`. The
//! expensive part, the HTTP connection pool, is process-wide in
//! `super::http::shared`.
//!
//! A provider is enabled exactly when its credentials exist. There is no
//! second per-provider switch to forget to flip.

pub mod discord;
pub mod github;
pub mod oauth2;

use std::collections::BTreeMap;

use async_trait::async_trait;

use naw_core::config::AuthConfig;

use super::http::HttpFetch;
use super::types::{AuthError, Identity, ProviderId};

pub struct AuthorizeParams<'a> {
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub code_challenge: Option<&'a str>,
    pub nonce: Option<&'a str>,
}

pub struct CompleteParams<'a> {
    /// The raw callback query. Kept as a map rather than typed fields because
    /// Steam's OpenID 2.0 flow needs every `openid.*` key verbatim.
    pub query: &'a BTreeMap<String, String>,
    pub redirect_uri: &'a str,
    pub code_verifier: Option<&'a str>,
    /// Part of the OIDC contract: the id_token's `nonce` claim must match the
    /// one sent to the authorize endpoint. GitHub and Discord take the
    /// userinfo route and never read it, Telegram will.
    #[allow(dead_code)]
    pub nonce: Option<&'a str>,
    pub http: &'a dyn HttpFetch,
}

impl CompleteParams<'_> {
    /// The authorization code, or the reason there is not one. A provider
    /// callback without `code` is either a user pressing cancel or a crawler
    /// hitting the URL, and neither is a 500.
    pub fn code(&self) -> Result<&str, AuthError> {
        if let Some(error) = self.query.get("error") {
            // access_denied is the spec's word for "the user said no".
            return Err(if error == "access_denied" {
                AuthError::Cancelled
            } else {
                AuthError::BadRequest("provider refused the authorization")
            });
        }
        self.query
            .get("code")
            .map(String::as_str)
            .filter(|code| !code.is_empty())
            .ok_or(AuthError::BadRequest("callback carried no code"))
    }
}

#[async_trait]
pub trait LoginProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    /// Button text on `/login`.
    fn label(&self) -> &'static str;

    fn authorize_url(&self, params: &AuthorizeParams<'_>) -> Result<String, AuthError>;

    async fn complete(&self, params: &CompleteParams<'_>) -> Result<Identity, AuthError>;
}

/// Builds the provider for `id`, or `None` when it has no credentials.
///
/// Returning `None` is what makes an unconfigured provider answer 404 instead
/// of starting a flow that cannot finish.
pub fn resolve(auth: &AuthConfig, id: ProviderId) -> Option<Box<dyn LoginProvider>> {
    match id {
        ProviderId::Github => auth
            .github
            .as_ref()
            .map(|creds| Box::new(github::Github::new(creds)) as Box<dyn LoginProvider>),
        ProviderId::Discord => auth
            .discord
            .as_ref()
            .map(|creds| Box::new(discord::Discord::new(creds)) as Box<dyn LoginProvider>),
        // Telegram OIDC, Google, Yandex, Twitch and Steam land in the next
        // slices. Until then they are honestly absent rather than half wired.
        ProviderId::Telegram
        | ProviderId::Google
        | ProviderId::Yandex
        | ProviderId::Twitch
        | ProviderId::Steam => None,
        // The dev provider does not go through the OAuth round trip at all,
        // it has its own handler.
        ProviderId::Dev => None,
    }
}

/// The providers a user can actually click right now, in display order.
pub fn enabled(auth: &AuthConfig) -> Vec<Box<dyn LoginProvider>> {
    [ProviderId::Github, ProviderId::Discord]
        .into_iter()
        .filter_map(|id| resolve(auth, id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use naw_core::config::OAuth2Creds;

    fn creds() -> OAuth2Creds {
        OAuth2Creds {
            client_id: "id".to_string(),
            client_secret: "secret".to_string(),
        }
    }

    #[test]
    fn a_provider_without_credentials_stays_absent() {
        let auth = AuthConfig::default();
        assert!(resolve(&auth, ProviderId::Github).is_none());
        assert!(resolve(&auth, ProviderId::Discord).is_none());
        assert!(enabled(&auth).is_empty());
    }

    #[test]
    fn credentials_alone_enable_a_provider() {
        let auth = AuthConfig {
            github: Some(creds()),
            ..AuthConfig::default()
        };
        let provider = resolve(&auth, ProviderId::Github).expect("github is configured");
        assert_eq!(provider.id(), ProviderId::Github);
        assert_eq!(enabled(&auth).len(), 1);
        assert!(resolve(&auth, ProviderId::Discord).is_none());
    }

    #[test]
    fn unimplemented_providers_are_none_even_with_credentials() {
        // Telegram credentials in config must not produce a half-wired flow.
        let auth = AuthConfig {
            telegram: Some(creds()),
            google: Some(creds()),
            ..AuthConfig::default()
        };
        assert!(resolve(&auth, ProviderId::Telegram).is_none());
        assert!(resolve(&auth, ProviderId::Google).is_none());
    }

    fn query(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    struct NoHttp;

    #[async_trait]
    impl HttpFetch for NoHttp {
        async fn post_form(
            &self,
            _url: &str,
            _form: &[(&str, &str)],
            _basic: Option<(&str, &str)>,
            _accept_json: bool,
        ) -> Result<super::super::http::FetchResponse, AuthError> {
            unreachable!("not called in these tests")
        }
        async fn get(
            &self,
            _url: &str,
            _bearer: Option<&str>,
            _headers: &[(&str, &str)],
        ) -> Result<super::super::http::FetchResponse, AuthError> {
            unreachable!("not called in these tests")
        }
    }

    fn params<'a>(q: &'a BTreeMap<String, String>, http: &'a NoHttp) -> CompleteParams<'a> {
        CompleteParams {
            query: q,
            redirect_uri: "https://wiki.test/auth/github/callback",
            code_verifier: None,
            nonce: None,
            http,
        }
    }

    #[test]
    fn code_extraction_separates_cancel_from_malformed() {
        let http = NoHttp;

        let good = query(&[("code", "abc123")]);
        assert_eq!(params(&good, &http).code().expect("code"), "abc123");

        let denied = query(&[("error", "access_denied")]);
        assert!(matches!(
            params(&denied, &http).code(),
            Err(AuthError::Cancelled)
        ));

        let other = query(&[("error", "server_error")]);
        assert!(matches!(
            params(&other, &http).code(),
            Err(AuthError::BadRequest(_))
        ));

        let empty = query(&[]);
        assert!(matches!(
            params(&empty, &http).code(),
            Err(AuthError::BadRequest(_))
        ));

        // A present but empty code is malformed, not a valid code.
        let blank = query(&[("code", "")]);
        assert!(matches!(
            params(&blank, &http).code(),
            Err(AuthError::BadRequest(_))
        ));
    }
}
