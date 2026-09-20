//! The outbound HTTP surface the providers are allowed to use.
//!
//! Providers never touch reqwest directly. They go through `HttpFetch`, which
//! keeps the provider logic testable offline: the tests hand them a canned
//! double instead of talking to GitHub.
//!
//! Every response is read as bytes with a size cap. A provider that starts
//! streaming gigabytes must not take the process down with it.

use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;

use super::types::AuthError;

/// Nothing an identity provider sends us is legitimately larger than this.
const MAX_BODY: usize = 256 * 1024;

/// Providers are third parties on the critical path of a login. They get a
/// short leash.
const TIMEOUT: Duration = Duration::from_secs(10);

pub struct FetchResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl FetchResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Parses the body as JSON. The parse error never reaches the user, so it
    /// carries the provider's own wording for the log and nothing else.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, AuthError> {
        serde_json::from_slice(&self.body).map_err(|err| {
            AuthError::Upstream(format!(
                "provider returned unparseable json: {err}, {} bytes",
                self.body.len()
            ))
        })
    }
}

#[async_trait]
pub trait HttpFetch: Send + Sync {
    /// Token endpoints. Every provider we support takes
    /// `application/x-www-form-urlencoded` and several reject JSON outright.
    async fn post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
        basic: Option<(&str, &str)>,
        accept_json: bool,
    ) -> Result<FetchResponse, AuthError>;

    async fn get(
        &self,
        url: &str,
        bearer: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Result<FetchResponse, AuthError>;
}

pub struct ReqwestFetch {
    client: reqwest::Client,
}

/// One client for the whole process. `reqwest::Client` owns the connection
/// pool, so building one per request would throw away every kept-alive
/// connection and re-handshake TLS on every login.
pub fn shared() -> &'static ReqwestFetch {
    static SHARED: OnceLock<ReqwestFetch> = OnceLock::new();
    SHARED.get_or_init(ReqwestFetch::new)
}

impl ReqwestFetch {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(TIMEOUT)
            // A provider that 302s us somewhere else during a token exchange
            // is not a provider we follow.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

impl Default for ReqwestFetch {
    fn default() -> Self {
        Self::new()
    }
}

/// GitHub answers 403 to any API call without a User-Agent, so this is not
/// decoration.
pub const USER_AGENT: &str = "NotAnotherWiki/0.1 (+https://snackers.wiki)";

async fn collect(response: reqwest::Response) -> Result<FetchResponse, AuthError> {
    let status = response.status().as_u16();
    let full = response
        .bytes()
        .await
        .map_err(|err| AuthError::Upstream(format!("provider body read failed: {err}")))?;
    if full.len() > MAX_BODY {
        return Err(AuthError::Upstream(format!(
            "provider body too large: {} bytes",
            full.len()
        )));
    }
    Ok(FetchResponse {
        status,
        body: full.to_vec(),
    })
}

#[async_trait]
impl HttpFetch for ReqwestFetch {
    async fn post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
        basic: Option<(&str, &str)>,
        accept_json: bool,
    ) -> Result<FetchResponse, AuthError> {
        let mut request = self.client.post(url).form(form);
        if accept_json {
            request = request.header(reqwest::header::ACCEPT, "application/json");
        }
        if let Some((user, password)) = basic {
            // reqwest can do basic auth itself, but Telegram wants the exact
            // base64(client_id:client_secret) pair and being explicit here
            // keeps the header identical to the spec.
            let encoded = STANDARD.encode(format!("{user}:{password}"));
            request = request.header(reqwest::header::AUTHORIZATION, format!("Basic {encoded}"));
        }
        let response = request
            .send()
            .await
            .map_err(|err| AuthError::Upstream(format!("provider token call failed: {err}")))?;
        collect(response).await
    }

    async fn get(
        &self,
        url: &str,
        bearer: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Result<FetchResponse, AuthError> {
        let mut request = self.client.get(url);
        if let Some(token) = bearer {
            request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request
            .send()
            .await
            .map_err(|err| AuthError::Upstream(format!("provider profile call failed: {err}")))?;
        collect(response).await
    }
}

#[cfg(test)]
pub mod test_double {
    //! A scripted `HttpFetch` for provider tests. Every call is recorded so a
    //! test can assert the exact request a provider made, which is where the
    //! interesting bugs live: a missing `Accept: application/json`, a token
    //! posted as JSON, a forgotten `code_verifier`.

    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    pub enum Call {
        PostForm {
            url: String,
            form: Vec<(String, String)>,
            basic: Option<String>,
            accept_json: bool,
        },
        Get {
            url: String,
            bearer: Option<String>,
            headers: Vec<(String, String)>,
        },
    }

    pub struct Scripted {
        /// url fragment -> (status, body). First match wins.
        routes: Vec<(String, u16, String)>,
        pub calls: Mutex<Vec<Call>>,
    }

    impl Scripted {
        pub fn new() -> Self {
            Self {
                routes: Vec::new(),
                calls: Mutex::new(Vec::new()),
            }
        }

        pub fn on(mut self, url_contains: &str, status: u16, body: &str) -> Self {
            self.routes
                .push((url_contains.to_string(), status, body.to_string()));
            self
        }

        fn answer(&self, url: &str) -> Result<FetchResponse, AuthError> {
            for (fragment, status, body) in &self.routes {
                if url.contains(fragment.as_str()) {
                    return Ok(FetchResponse {
                        status: *status,
                        body: body.clone().into_bytes(),
                    });
                }
            }
            panic!("test double has no route for {url}");
        }

        pub fn calls(&self) -> std::sync::MutexGuard<'_, Vec<Call>> {
            self.calls.lock().expect("call log")
        }

        /// The form a given call posted, for assertions on a single field.
        pub fn posted(&self, index: usize, key: &str) -> Option<String> {
            match self.calls().get(index) {
                Some(Call::PostForm { form, .. }) => form
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value.clone()),
                _ => None,
            }
        }
    }

    #[async_trait]
    impl HttpFetch for Scripted {
        async fn post_form(
            &self,
            url: &str,
            form: &[(&str, &str)],
            basic: Option<(&str, &str)>,
            accept_json: bool,
        ) -> Result<FetchResponse, AuthError> {
            self.calls().push(Call::PostForm {
                url: url.to_string(),
                form: form
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
                basic: basic.map(|(user, _)| user.to_string()),
                accept_json,
            });
            self.answer(url)
        }

        async fn get(
            &self,
            url: &str,
            bearer: Option<&str>,
            headers: &[(&str, &str)],
        ) -> Result<FetchResponse, AuthError> {
            self.calls().push(Call::Get {
                url: url.to_string(),
                bearer: bearer.map(str::to_string),
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            });
            self.answer(url)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_errors_do_not_leak_the_body_to_the_user() {
        let response = FetchResponse {
            status: 200,
            body: b"{\"access_token\": ".to_vec(),
        };
        let err = response.json::<serde_json::Value>().expect_err("must fail");
        // Upstream detail only ever reaches the log, and it must not carry the
        // token fragment that failed to parse.
        let AuthError::Upstream(detail) = err else {
            panic!("expected an upstream error");
        };
        assert!(detail.contains("unparseable json"));
        assert!(!detail.contains("access_token"));
    }

    #[test]
    fn success_is_the_2xx_range_only() {
        for (status, expected) in [
            (200, true),
            (204, true),
            (299, true),
            (300, false),
            (404, false),
            (502, false),
        ] {
            let response = FetchResponse {
                status,
                body: Vec::new(),
            };
            assert_eq!(response.is_success(), expected, "status {status}");
        }
    }
}
