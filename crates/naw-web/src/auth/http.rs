//! Outbound HTTP for the providers, behind `HttpFetch` so provider logic is
//! testable offline. Every body is read with a size cap.

use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;

use super::types::AuthError;

/// Largest body accepted from an identity provider.
const MAX_BODY: usize = 256 * 1024;

/// Providers are on the critical path of a login.
const TIMEOUT: Duration = Duration::from_secs(10);

pub struct FetchResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl FetchResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Parses the body as JSON; the error is for the log only.
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
    /// Token endpoints, all form-encoded.
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

/// One client and connection pool for the process.
pub fn shared() -> &'static ReqwestFetch {
    static SHARED: OnceLock<ReqwestFetch> = OnceLock::new();
    SHARED.get_or_init(ReqwestFetch::new)
}

impl ReqwestFetch {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(TIMEOUT)
            // A provider that redirects a token exchange is not followed.
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

/// GitHub refuses API calls without a User-Agent.
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
            // Telegram expects exactly base64(client_id:client_secret).
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
    //! A scripted `HttpFetch` that records every call, so tests can assert the
    //! exact request a provider made.

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
        /// url fragment -> (status, body); the first match wins.
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

        /// The form a given call posted.
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
        // Upstream detail must not carry the token fragment.
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
