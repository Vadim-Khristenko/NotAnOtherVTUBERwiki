//! The shared parts of the authorization code flow.

use serde::Deserialize;

use crate::auth::http::HttpFetch;
use crate::auth::types::AuthError;

use super::AuthorizeParams;

/// The fields used; tokens live for one callback and are never stored.
#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: Option<String>,
    pub id_token: Option<String>,
    /// Providers report failure in the body as often as in the status.
    pub error: Option<String>,
    pub error_description: Option<String>,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Both tokens are credentials.
        f.debug_struct("TokenResponse")
            .field("access_token", &presence(&self.access_token))
            .field("id_token", &presence(&self.id_token))
            .field("error", &self.error)
            .field("error_description", &self.error_description)
            .finish()
    }
}

fn presence<T>(value: &Option<T>) -> &'static str {
    if value.is_some() { "set" } else { "unset" }
}

impl TokenResponse {
    /// The bearer token, or an upstream error for the log.
    pub fn bearer(self) -> Result<String, AuthError> {
        if let Some(error) = self.error {
            let detail = self.error_description.unwrap_or_default();
            return Err(AuthError::Upstream(format!(
                "token endpoint refused: {error} {detail}"
            )));
        }
        self.access_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AuthError::Upstream("token endpoint returned no access_token".into()))
    }
}

/// The authorize URL, every value percent-encoded once by `Url`.
pub fn authorize_url(
    endpoint: &str,
    client_id: &str,
    scope: &str,
    params: &AuthorizeParams<'_>,
    extra: &[(&str, &str)],
) -> Result<String, AuthError> {
    let mut url = reqwest::Url::parse(endpoint)
        .map_err(|err| AuthError::Upstream(format!("bad authorize endpoint: {err}")))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", params.redirect_uri);
        query.append_pair("response_type", "code");
        query.append_pair("scope", scope);
        query.append_pair("state", params.state);
        if let Some(challenge) = params.code_challenge {
            query.append_pair("code_challenge", challenge);
            // S256 only.
            query.append_pair("code_challenge_method", "S256");
        }
        if let Some(nonce) = params.nonce {
            query.append_pair("nonce", nonce);
        }
        for (key, value) in extra {
            query.append_pair(key, value);
        }
    }
    Ok(url.to_string())
}

/// One token exchange; the flags are provider quirks.
pub struct Exchange<'a> {
    pub token_endpoint: &'a str,
    pub client_id: &'a str,
    pub client_secret: &'a str,
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub code_verifier: Option<&'a str>,
    /// GitHub answers form-encoded unless asked for JSON.
    pub accept_json: bool,
    /// Telegram wants the client pair in an Authorization header.
    pub use_basic_auth: bool,
}

/// Trades the authorization code for a token.
pub async fn exchange_code(
    http: &dyn HttpFetch,
    request: &Exchange<'_>,
) -> Result<TokenResponse, AuthError> {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", request.code),
        ("redirect_uri", request.redirect_uri),
        ("client_id", request.client_id),
    ];
    if !request.use_basic_auth {
        form.push(("client_secret", request.client_secret));
    }
    if let Some(verifier) = request.code_verifier {
        form.push(("code_verifier", verifier));
    }
    let basic = if request.use_basic_auth {
        Some((request.client_id, request.client_secret))
    } else {
        None
    };
    let response = http
        .post_form(request.token_endpoint, &form, basic, request.accept_json)
        .await?;
    // Error responses often carry JSON; let `bearer()` decide.
    if !response.is_success() && response.body.is_empty() {
        return Err(AuthError::Upstream(format!(
            "token endpoint returned {} with an empty body",
            response.status
        )));
    }
    response.json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::http::test_double::{Call, Scripted};

    fn params<'a>(state: &'a str, challenge: Option<&'a str>) -> AuthorizeParams<'a> {
        AuthorizeParams {
            redirect_uri: "https://snackers.wiki/auth/github/callback",
            state,
            code_challenge: challenge,
            nonce: None,
        }
    }

    #[test]
    fn authorize_url_encodes_every_value_once() {
        let url = authorize_url(
            "https://github.com/login/oauth/authorize",
            "client-id",
            "read:user user:email",
            &params("st-ate", Some("chal-lenge")),
            &[],
        )
        .expect("builds");

        assert!(url.contains("scope=read%3Auser+user%3Aemail"));
        assert!(
            url.contains("redirect_uri=https%3A%2F%2Fsnackers.wiki%2Fauth%2Fgithub%2Fcallback")
        );
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state=st-ate"));
        assert!(url.contains("code_challenge=chal-lenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(!url.contains("%253A"), "double encoded: {url}");
    }

    #[test]
    fn a_hostile_state_cannot_inject_extra_parameters() {
        let url = authorize_url(
            "https://github.com/login/oauth/authorize",
            "client-id",
            "read:user",
            &params("evil&redirect_uri=https://attacker.test", None),
            &[],
        )
        .expect("builds");
        assert_eq!(url.matches("redirect_uri=").count(), 1);
        assert!(url.contains("evil%26redirect_uri%3D"));
        assert!(!url.contains("code_challenge"));
    }

    /// A GitHub-shaped exchange.
    fn github_exchange<'a>(code: &'a str, verifier: Option<&'a str>) -> Exchange<'a> {
        Exchange {
            token_endpoint: "https://github.com/login/oauth/access_token",
            client_id: "client-id",
            client_secret: "client-secret",
            code,
            redirect_uri: "https://snackers.wiki/auth/github/callback",
            code_verifier: verifier,
            accept_json: true,
            use_basic_auth: false,
        }
    }

    #[tokio::test]
    async fn exchange_posts_a_form_and_asks_for_json() {
        let http = Scripted::new().on(
            "oauth/access_token",
            200,
            r#"{"access_token":"gho_token","token_type":"bearer"}"#,
        );
        let token = exchange_code(&http, &github_exchange("the-code", Some("the-verifier")))
            .await
            .expect("exchange")
            .bearer()
            .expect("bearer");

        assert_eq!(token, "gho_token");
        assert_eq!(http.posted(0, "code").as_deref(), Some("the-code"));
        assert_eq!(
            http.posted(0, "code_verifier").as_deref(),
            Some("the-verifier")
        );
        assert_eq!(
            http.posted(0, "client_secret").as_deref(),
            Some("client-secret")
        );
        match &http.calls()[0] {
            Call::PostForm {
                accept_json, basic, ..
            } => {
                assert!(accept_json, "GitHub answers form-encoded without it");
                assert!(basic.is_none(), "GitHub takes the secret in the body");
            }
            other => panic!("expected a form post, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn basic_auth_moves_the_secret_out_of_the_body() {
        let http = Scripted::new().on("token", 200, r#"{"access_token":"tg","id_token":"jwt"}"#);
        exchange_code(
            &http,
            &Exchange {
                token_endpoint: "https://oauth.telegram.org/token",
                client_id: "bot-id",
                client_secret: "bot-secret",
                code: "code",
                redirect_uri: "https://snackers.wiki/auth/telegram/callback",
                code_verifier: Some("v"),
                accept_json: false,
                use_basic_auth: true,
            },
        )
        .await
        .expect("exchange");

        assert!(
            http.posted(0, "client_secret").is_none(),
            "secret must not be duplicated into the form"
        );
        match &http.calls()[0] {
            Call::PostForm { basic, .. } => assert_eq!(basic.as_deref(), Some("bot-id")),
            other => panic!("expected a form post, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_body_level_error_becomes_upstream_not_a_panic() {
        let http = Scripted::new().on(
            "access_token",
            200,
            r#"{"error":"bad_verification_code","error_description":"code expired"}"#,
        );
        let err = exchange_code(&http, &github_exchange("stale", None))
            .await
            .expect("http call succeeds")
            .bearer()
            .expect_err("but the body is an error");

        let AuthError::Upstream(detail) = err else {
            panic!("expected upstream");
        };
        assert!(detail.contains("bad_verification_code"));
    }

    #[tokio::test]
    async fn an_empty_non_2xx_body_is_reported_rather_than_parsed() {
        let http = Scripted::new().on("access_token", 503, "");
        let err = exchange_code(&http, &github_exchange("code", None))
            .await
            .expect_err("must not try to parse an empty body");
        assert!(matches!(err, AuthError::Upstream(_)));
    }
}
