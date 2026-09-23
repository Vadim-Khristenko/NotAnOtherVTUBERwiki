//! The provider registry and the contract every login backend implements.
//! A provider is enabled exactly when its credentials exist.

pub mod discord;
pub mod github;
pub mod oauth2;
pub mod telegram;

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
    /// The raw callback query; Steam's OpenID needs every `openid.*` key.
    pub query: &'a BTreeMap<String, String>,
    pub redirect_uri: &'a str,
    pub code_verifier: Option<&'a str>,
    /// The nonce sent to the authorize endpoint, for the id_token check.
    pub nonce: Option<&'a str>,
    /// Where a JWKS may be cached; `None` fetches every time.
    pub cache: Option<&'a deadpool_redis::Pool>,
    pub http: &'a dyn HttpFetch,
}

impl CompleteParams<'_> {
    /// The authorization code; a callback without one was cancelled or crawled.
    pub fn code(&self) -> Result<&str, AuthError> {
        if let Some(error) = self.query.get("error") {
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

/// The provider for `id`, or `None` without credentials, which makes it 404.
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
        ProviderId::Telegram => auth
            .telegram
            .as_ref()
            .map(|creds| Box::new(telegram::Telegram::new(creds)) as Box<dyn LoginProvider>),
        // Not implemented yet.
        ProviderId::Google | ProviderId::Yandex | ProviderId::Twitch | ProviderId::Steam => None,
        // The dev provider has its own handler.
        ProviderId::Dev => None,
    }
}

/// The providers a user can click now, in display order.
pub fn enabled(auth: &AuthConfig) -> Vec<Box<dyn LoginProvider>> {
    [
        ProviderId::Github,
        ProviderId::Discord,
        ProviderId::Telegram,
    ]
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
        let auth = AuthConfig {
            google: Some(creds()),
            yandex: Some(creds()),
            twitch: Some(creds()),
            ..AuthConfig::default()
        };
        assert!(resolve(&auth, ProviderId::Google).is_none());
        assert!(resolve(&auth, ProviderId::Yandex).is_none());
        assert!(resolve(&auth, ProviderId::Twitch).is_none());
        assert!(resolve(&auth, ProviderId::Steam).is_none());
    }

    #[test]
    fn all_three_round_one_providers_are_wired() {
        let auth = AuthConfig {
            github: Some(creds()),
            discord: Some(creds()),
            telegram: Some(creds()),
            ..AuthConfig::default()
        };
        let labels: Vec<_> = enabled(&auth)
            .iter()
            .map(|provider| provider.label())
            .collect();
        assert_eq!(labels, vec!["GitHub", "Discord", "Telegram"]);
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
            cache: None,
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

        let blank = query(&[("code", "")]);
        assert!(matches!(
            params(&blank, &http).code(),
            Err(AuthError::BadRequest(_))
        ));
    }
}
