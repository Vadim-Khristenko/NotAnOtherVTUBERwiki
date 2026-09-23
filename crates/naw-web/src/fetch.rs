//! Outside fetches: emote files, images imported by URL.
//!
//! Every fetch goes direct first. When the direct path fails or stalls (a
//! hosting range throttled on the way can freeze a connection after a few
//! kilobytes), it is retried through `fetch_proxy`, and the host is sent
//! through the proxy first for a while after.
//!
//! Nothing here may reach a private address. The resolver drops every
//! address that is not public, which covers redirects and the proxy path as
//! well (a `socks5://` proxy is given the address resolved here), and IP
//! literals are checked before a request and on every redirect.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use naw_core::state::AppState;

/// Longest wait for a connection, and for the next chunk of a body.
const STALL: Duration = Duration::from_secs(6);
/// Longest one attempt may take in all.
const ATTEMPT_MAX: Duration = Duration::from_secs(45);
/// How long a host that needed the proxy goes through it first.
const REMEMBER: Duration = Duration::from_secs(6 * 3600);
const REDIRECTS_MAX: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// Not an http(s) URL on a standard port, or it points somewhere private.
    Refused,
    TooLarge,
    Status(u16),
    /// Neither path delivered.
    Failed(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused => f.write_str("address refused"),
            Self::TooLarge => f.write_str("too large"),
            Self::Status(code) => write!(f, "answered {code}"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// A fetched body. Its type is decided by the caller from the bytes.
pub struct Fetched {
    pub bytes: Vec<u8>,
}

/// Whether `ip` is on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let seg = v6.segments();
            let internal = v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80 // link local
                || (seg[0] & 0xfe00) == 0x0200 // Yggdrasil and other 200::/7 overlays
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
                || (seg[0] == 0x0064 && seg[1] == 0xff9b) // NAT64 maps to IPv4
                || seg[0] == 0x2002; // 6to4 embeds IPv4
            !internal
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || a >= 240
        || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT
        || (a == 198 && (18..20).contains(&b)) // benchmarking
        || (a == 192 && b == 0 && ip.octets()[2] == 0)) // IETF protocol assignments
}

/// Resolves names and keeps only public addresses.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| is_public(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// An http(s) URL on port 80 or 443, without credentials, whose host is not
/// a private IP literal. Names are checked by the resolver when connecting.
pub fn check_url(raw: &str) -> Result<reqwest::Url, FetchError> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| FetchError::Refused)?;
    url_allowed(&url).then_some(url).ok_or(FetchError::Refused)
}

fn url_allowed(url: &reqwest::Url) -> bool {
    let scheme_ok = matches!(url.scheme(), "http" | "https");
    let port_ok = matches!(url.port_or_known_default(), Some(80 | 443));
    let host_ok = match url.host() {
        Some(url::Host::Ipv4(ip)) => is_public_v4(ip),
        Some(url::Host::Ipv6(ip)) => is_public(IpAddr::V6(ip)),
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            !(name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local"))
        }
        None => false,
    };
    scheme_ok && port_ok && host_ok && url.username().is_empty() && url.password().is_none()
}

fn build(proxy: Option<&str>) -> reqwest::Client {
    let redirects = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= REDIRECTS_MAX || !url_allowed(attempt.url()) {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    let mut builder = reqwest::Client::builder()
        .dns_resolver(std::sync::Arc::new(PublicOnly))
        .connect_timeout(STALL)
        .timeout(ATTEMPT_MAX)
        .redirect(redirects)
        .user_agent(crate::auth::http::USER_AGENT)
        .no_proxy();
    if let Some(proxy) = proxy.and_then(|p| reqwest::Proxy::all(p).ok()) {
        builder = builder.proxy(proxy);
    }
    builder.build().unwrap_or_default()
}

struct Clients {
    direct: reqwest::Client,
    proxied: Option<reqwest::Client>,
}

fn clients(state: &AppState) -> &'static Clients {
    static CLIENTS: OnceLock<Clients> = OnceLock::new();
    CLIENTS.get_or_init(|| Clients {
        direct: build(None),
        proxied: state.config.fetch_proxy.as_deref().map(|p| build(Some(p))),
    })
}

fn proxy_hosts() -> &'static Mutex<HashMap<String, Instant>> {
    static HOSTS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    HOSTS.get_or_init(Default::default)
}

fn prefers_proxy(host: &str) -> bool {
    let mut hosts = proxy_hosts().lock().unwrap_or_else(|e| e.into_inner());
    hosts.retain(|_, since| since.elapsed() < REMEMBER);
    hosts.contains_key(host)
}

fn remember_proxy(host: &str) {
    let mut hosts = proxy_hosts().lock().unwrap_or_else(|e| e.into_inner());
    hosts.insert(host.to_string(), Instant::now());
}

async fn attempt(
    client: &reqwest::Client,
    url: &reqwest::Url,
    max: usize,
) -> Result<Fetched, FetchError> {
    let mut response = tokio::time::timeout(STALL * 2, client.get(url.clone()).send())
        .await
        .map_err(|_| FetchError::Failed("no answer".into()))?
        .map_err(|e| FetchError::Failed(e.without_url().to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|len| len as usize > max)
    {
        return Err(FetchError::TooLarge);
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::time::timeout(STALL, response.chunk())
            .await
            .map_err(|_| FetchError::Failed("the transfer stalled".into()))?
            .map_err(|e| FetchError::Failed(e.without_url().to_string()))?;
        let Some(chunk) = chunk else { break };
        if bytes.len() + chunk.len() > max {
            return Err(FetchError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched { bytes })
}

/// GETs `url`, at most `max` bytes, direct first and through the proxy
/// when direct fails. A refusal, a size or an HTTP status is final.
pub async fn get(state: &AppState, url: &str, max: usize) -> Result<Fetched, FetchError> {
    let url = check_url(url)?;
    let host = url.host_str().unwrap_or_default().to_string();
    let clients = clients(state);
    let proxy_first = clients.proxied.is_some() && prefers_proxy(&host);
    let order: Vec<(&reqwest::Client, bool)> = match &clients.proxied {
        Some(proxied) if proxy_first => vec![(proxied, true), (&clients.direct, false)],
        Some(proxied) => vec![(&clients.direct, false), (proxied, true)],
        None => vec![(&clients.direct, false)],
    };
    let mut last = FetchError::Failed("not attempted".into());
    for (client, via_proxy) in order {
        match attempt(client, &url, max).await {
            Ok(fetched) => {
                if via_proxy {
                    remember_proxy(&host);
                }
                return Ok(fetched);
            }
            Err(err @ (FetchError::Refused | FetchError::TooLarge | FetchError::Status(_))) => {
                return Err(err);
            }
            Err(err) => {
                tracing::debug!(%host, via_proxy, error = %err, "fetch attempt failed");
                last = err;
            }
        }
    }
    Err(last)
}

/// [`get`] and parse as JSON.
pub async fn get_json(
    state: &AppState,
    url: &str,
    max: usize,
) -> Result<serde_json::Value, FetchError> {
    let fetched = get(state, url, max).await?;
    serde_json::from_slice(&fetched.bytes).map_err(|_| FetchError::Failed("not JSON".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_special_addresses_are_refused() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.31.32",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "198.18.0.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "200:ec90::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "2002:a00:1::1",
            "2001:db8::1",
        ] {
            assert!(!is_public(bad.parse().unwrap()), "{bad}");
        }
        for good in ["95.217.175.63", "1.1.1.1", "2a01:4f9:c010::1"] {
            assert!(is_public(good.parse().unwrap()), "{good}");
        }
    }

    #[test]
    fn only_plain_public_web_urls_pass() {
        assert!(check_url("https://cdn.7tv.app/emote/x/2x.webp").is_ok());
        assert!(check_url("http://example.com/a.png").is_ok());
        for bad in [
            "ftp://example.com/a",
            "file:///etc/passwd",
            "https://127.0.0.1/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data",
            "https://example.com:8080/a",
            "https://user:pass@example.com/a",
            "http://localhost/",
            "http://printer.local/",
            "gopher://example.com",
            "not a url",
        ] {
            assert_eq!(check_url(bad).err(), Some(FetchError::Refused), "{bad}");
        }
    }
}
