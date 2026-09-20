//! Discord, OAuth2 with an OIDC userinfo endpoint.
//!
//! The token endpoint accepts only `application/x-www-form-urlencoded` and
//! rejects JSON outright. PKCE is S256 only, `plain` is refused.
//!
//! Identity comes from the OIDC userinfo call rather than `/users/@me`: it is
//! one request, it carries `email_verified` as a real boolean, and `sub` is the
//! same stable snowflake either way.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use naw_core::config::OAuth2Creds;

use crate::auth::types::{AuthError, Identity, ProviderId};

use super::oauth2;
use super::{AuthorizeParams, CompleteParams, LoginProvider};

const AUTHORIZE: &str = "https://discord.com/oauth2/authorize";
const TOKEN: &str = "https://discord.com/api/oauth2/token";
const USERINFO: &str = "https://discord.com/api/v10/oauth2/userinfo";
const SCOPE: &str = "openid identify email";

pub struct Discord {
    client_id: String,
    client_secret: String,
}

impl Discord {
    pub fn new(creds: &OAuth2Creds) -> Self {
        Self {
            client_id: creds.client_id.clone(),
            client_secret: creds.client_secret.clone(),
        }
    }
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
    preferred_username: Option<String>,
    nickname: Option<String>,
    picture: Option<String>,
}

#[async_trait]
impl LoginProvider for Discord {
    fn id(&self) -> ProviderId {
        ProviderId::Discord
    }

    fn label(&self) -> &'static str {
        "Discord"
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
                // Discord answers JSON regardless, and refuses a JSON body.
                accept_json: false,
                use_basic_auth: false,
            },
        )
        .await?
        .bearer()?;

        let response = params.http.get(USERINFO, Some(&bearer), &[]).await?;
        if !response.is_success() {
            return Err(AuthError::Upstream(format!(
                "discord userinfo returned {}",
                response.status
            )));
        }
        let info: UserInfo = response.json()?;
        if info.sub.trim().is_empty() {
            return Err(AuthError::Upstream(
                "discord userinfo carried no sub".to_string(),
            ));
        }

        // Only a confirmed address counts. store.rs treats a present email as
        // provider certified, so an unconfirmed one must arrive as None.
        let email = info
            .email
            .filter(|_| info.email_verified)
            .map(|address| address.trim().to_lowercase())
            .filter(|address| !address.is_empty());

        let handle = info
            .preferred_username
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("discord-{}", info.sub));

        Ok(Identity {
            provider: ProviderId::Discord,
            provider_user_id: info.sub.clone(),
            email_verified: email.is_some(),
            email,
            display_name: info.nickname.filter(|name| !name.trim().is_empty()),
            avatar_url: info.picture,
            handle,
            raw: json!({ "sub": info.sub, "preferred_username": info.preferred_username }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::http::test_double::{Call, Scripted};

    fn provider() -> Discord {
        Discord::new(&OAuth2Creds {
            client_id: "1234567890".to_string(),
            client_secret: "discord-secret".to_string(),
        })
    }

    fn query(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    async fn complete_with(userinfo: &str) -> Result<Identity, AuthError> {
        let http = Scripted::new()
            .on("oauth2/token", 200, r#"{"access_token":"disc_tok"}"#)
            .on("oauth2/userinfo", 200, userinfo);
        let q = query(&[("code", "the-code")]);
        provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/discord/callback",
                code_verifier: Some("verifier"),
                nonce: None,
                cache: None,
                http: &http,
            })
            .await
    }

    #[tokio::test]
    async fn a_verified_address_is_taken_and_lowercased() {
        let identity = complete_with(
            r#"{"sub":"80351110224678912","email":"Snacker@Example.test",
                "email_verified":true,"preferred_username":"snacker",
                "nickname":"Snacker","picture":"https://cdn.discordapp.com/avatars/x.png"}"#,
        )
        .await
        .expect("completes");

        assert_eq!(identity.provider_user_id, "80351110224678912");
        assert_eq!(identity.handle, "snacker");
        assert_eq!(identity.email.as_deref(), Some("snacker@example.test"));
        assert!(identity.email_verified);
        assert!(!identity.raw.to_string().contains("disc_tok"));
        assert!(
            !identity
                .raw
                .to_string()
                .to_lowercase()
                .contains("example.test"),
            "the address must stay out of the stored profile"
        );
    }

    #[tokio::test]
    async fn an_unconfirmed_address_arrives_as_none() {
        let identity = complete_with(
            r#"{"sub":"1","email":"victim@example.test","email_verified":false,
                "preferred_username":"imposter"}"#,
        )
        .await
        .expect("completes");
        assert_eq!(identity.email, None);
        assert!(!identity.email_verified);
    }

    #[tokio::test]
    async fn a_missing_email_verified_field_defaults_to_unverified() {
        // serde(default) on a bool is false, and that is the safe direction.
        let identity = complete_with(r#"{"sub":"2","email":"x@example.test"}"#)
            .await
            .expect("completes");
        assert_eq!(identity.email, None);
    }

    #[tokio::test]
    async fn a_handle_is_always_produced_even_without_a_username() {
        let identity = complete_with(r#"{"sub":"99","preferred_username":"  "}"#)
            .await
            .expect("completes");
        // username::sanitize would otherwise get an empty string and fall back
        // to a random name; a stable provider-derived handle is kinder.
        assert_eq!(identity.handle, "discord-99");
    }

    #[tokio::test]
    async fn an_empty_sub_is_refused_rather_than_stored() {
        let err = complete_with(r#"{"sub":"   ","email_verified":true}"#)
            .await
            .expect_err("a login with no subject is not a login");
        assert!(matches!(err, AuthError::Upstream(_)));
    }

    #[tokio::test]
    async fn the_token_call_is_form_encoded_and_carries_the_verifier() {
        let http = Scripted::new()
            .on("oauth2/token", 200, r#"{"access_token":"t"}"#)
            .on("oauth2/userinfo", 200, r#"{"sub":"1"}"#);
        let q = query(&[("code", "c")]);
        provider()
            .complete(&CompleteParams {
                query: &q,
                redirect_uri: "https://snackers.wiki/auth/discord/callback",
                code_verifier: Some("v"),
                nonce: None,
                cache: None,
                http: &http,
            })
            .await
            .expect("completes");

        assert_eq!(http.posted(0, "code_verifier").as_deref(), Some("v"));
        assert_eq!(
            http.posted(0, "grant_type").as_deref(),
            Some("authorization_code")
        );
        match &http.calls()[0] {
            Call::PostForm { basic, .. } => {
                assert!(basic.is_none(), "discord wants the secret in the body");
            }
            other => panic!("expected a form post, got {other:?}"),
        }
    }

    #[test]
    fn the_authorize_url_requests_openid_and_s256() {
        let url = provider()
            .authorize_url(&AuthorizeParams {
                redirect_uri: "https://snackers.wiki/auth/discord/callback",
                state: "st",
                code_challenge: Some("ch"),
                nonce: None,
            })
            .expect("builds");
        assert!(url.starts_with(AUTHORIZE));
        assert!(url.contains("scope=openid+identify+email"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(!url.contains("discord-secret"));
    }
}
