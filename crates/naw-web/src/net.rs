//! Who is on the other end of a request.
//!
//! Behind a reverse proxy or a tunnel every connection comes from loopback, so
//! the peer address alone would put the whole internet into one rate limit
//! bucket. The proxy's headers are trusted only when the operator said there is
//! a proxy (`trust_proxy`) and the connection really comes from loopback, so a
//! client talking to the engine directly cannot pick its own address.

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

/// The client address for rate limits and the session log.
pub fn client_ip(headers: &HeaderMap, peer: SocketAddr, trust_proxy: bool) -> IpAddr {
    let peer_ip = peer.ip();
    if !trust_proxy || !peer_ip.is_loopback() {
        return peer_ip;
    }
    // nginx sets X-Real-IP to the address it saw. X-Forwarded-For is a list the
    // client can prefill, so only its last entry, the one our proxy appended,
    // counts.
    let real_ip = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<IpAddr>().ok());
    let forwarded = || {
        headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit(',').next())
            .and_then(|value| value.trim().parse::<IpAddr>().ok())
    };
    real_ip.or_else(forwarded).unwrap_or(peer_ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().expect("header value"));
        }
        map
    }

    const LOOPBACK: &str = "127.0.0.1:50000";
    const OUTSIDE: &str = "203.0.113.9:50000";

    #[test]
    fn headers_are_ignored_unless_a_proxy_is_declared() {
        let h = headers(&[("x-real-ip", "198.51.100.7")]);
        let ip = client_ip(&h, LOOPBACK.parse().unwrap(), false);
        assert_eq!(ip, "127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn a_direct_client_cannot_choose_its_address() {
        let h = headers(&[("x-real-ip", "198.51.100.7")]);
        let ip = client_ip(&h, OUTSIDE.parse().unwrap(), true);
        assert_eq!(ip, "203.0.113.9".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn behind_the_proxy_the_real_address_wins() {
        let h = headers(&[("x-real-ip", "198.51.100.7")]);
        let ip = client_ip(&h, LOOPBACK.parse().unwrap(), true);
        assert_eq!(ip, "198.51.100.7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn only_the_last_forwarded_hop_counts() {
        let h = headers(&[("x-forwarded-for", "10.0.0.1, 198.51.100.8")]);
        let ip = client_ip(&h, LOOPBACK.parse().unwrap(), true);
        assert_eq!(ip, "198.51.100.8".parse::<IpAddr>().unwrap());
        let junk = headers(&[("x-forwarded-for", "not an address")]);
        let ip = client_ip(&junk, LOOPBACK.parse().unwrap(), true);
        assert_eq!(ip, "127.0.0.1".parse::<IpAddr>().unwrap());
    }
}
