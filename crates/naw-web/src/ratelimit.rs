//! A rate limit per client address on every route that costs something.
//!
//! Requests fall into four classes, each with its own budget a minute from
//! `naw_core::limits`: page reads, heavy reads (search, history, diffs, old
//! revisions), typing (previews, draft saves, suggestions) and other forms.
//! Each budget is a bucket that holds a minute's worth and refills evenly, so a
//! burst passes and a steady flood does not.
//!
//! The buckets live in this process, not in Valkey: a check is a hash lookup
//! and works with Valkey down. With several processes each keeps its own, and
//! the effective budget grows with their number.
//!
//! The layer runs before the session and the error pages, so a request turned
//! away costs no database work.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Method, Request};
use axum::middleware::Next;
use axum::response::Response;

use naw_core::limits::Limits;
use naw_core::state::AppState;

use crate::errors::RequestId;

/// What a request costs, which picks its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Read,
    Heavy,
    Typing,
    Write,
}

impl Class {
    fn per_minute(self, limits: &Limits) -> i64 {
        match self {
            Self::Read => limits.rate_read_per_minute,
            Self::Heavy => limits.rate_heavy_per_minute,
            Self::Typing => limits.rate_typing_per_minute,
            Self::Write => limits.rate_write_per_minute,
        }
    }
}

/// Requests nobody needs to count: static files, images, health checks, and
/// the Discord endpoint, whose requests come from Discord and are signed.
fn is_exempt(path: &str) -> bool {
    matches!(
        path,
        "/health"
            | "/ready"
            | "/favicon.ico"
            | "/favicon-96x96.png"
            | "/apple-touch-icon.png"
            | "/site.webmanifest"
            | "/robots.txt"
            | "/emote-cache.js"
            | "/discord/interactions"
    ) || path.starts_with("/skin/")
        // A stored file, /media/{prefix}/{file}; the lists and forms under
        // /media are counted.
        || (path.starts_with("/media/") && path.split('/').count() == 4)
}

/// Reads that query more than a cached page does.
fn is_heavy(path: &str) -> bool {
    path == "/search"
        || path == "/media"
        || path.ends_with("/history")
        || path.ends_with("/diff")
        || path.contains("/rev/")
}

/// The class of a request, or `None` when it is not counted. `path` is the
/// routed one, with any language prefix already taken off.
pub fn classify(method: &Method, path: &str) -> Option<Class> {
    if is_exempt(path) {
        return None;
    }
    if method == Method::GET || method == Method::HEAD {
        return Some(if path == "/api/complete" {
            Class::Typing
        } else if is_heavy(path) {
            Class::Heavy
        } else {
            Class::Read
        });
    }
    if method == Method::OPTIONS {
        return None;
    }
    Some(if path == "/preview" || path == "/drafts/save" {
        Class::Typing
    } else {
        Class::Write
    })
}

/// Whom a bucket belongs to: an IPv4 address, or an IPv6 /64, since one
/// machine is usually given a whole /64 and could walk through it.
fn owner(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
    /// The last request was turned away.
    limited: bool,
}

/// A request turned away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    /// Seconds until the next request would pass.
    pub retry_after: u64,
    /// The first refusal since this bucket last let one through, which is
    /// the one worth a log line; a flood logs once, not per request.
    pub first: bool,
}

/// Buckets kept before idle ones are swept.
const SWEEP_AT: usize = 50_000;
/// Buckets kept at most; past it, after a sweep, all are forgotten, which
/// lets everyone through for a moment rather than growing without end.
const HARD_CAP: usize = 200_000;
/// A bucket idle this long is full again, so forgetting it changes nothing.
const IDLE: Duration = Duration::from_secs(60);

/// The buckets of one router.
#[derive(Default)]
pub struct Limiter {
    buckets: Mutex<HashMap<(Class, IpAddr), Bucket>>,
}

impl Limiter {
    /// Takes one request from the bucket of `ip` for `class`.
    pub fn check(
        &self,
        class: Class,
        ip: IpAddr,
        per_minute: i64,
        now: Instant,
    ) -> Result<(), Refused> {
        let capacity = per_minute.max(1) as f64;
        let per_sec = capacity / 60.0;
        let Ok(mut buckets) = self.buckets.lock() else {
            // A panic while holding the lock: let traffic through.
            return Ok(());
        };
        if buckets.len() >= SWEEP_AT {
            buckets.retain(|_, b| now.saturating_duration_since(b.last) < IDLE);
            if buckets.len() >= HARD_CAP {
                tracing::warn!(
                    buckets = buckets.len(),
                    "rate limit table full, starting over"
                );
                buckets.clear();
            }
        }
        let bucket = buckets.entry((class, owner(ip))).or_insert(Bucket {
            tokens: capacity,
            last: now,
            limited: false,
        });
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * per_sec).min(capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            bucket.limited = false;
            Ok(())
        } else {
            let first = !bucket.limited;
            bucket.limited = true;
            Err(Refused {
                retry_after: ((1.0 - bucket.tokens) / per_sec).ceil().max(1.0) as u64,
                first,
            })
        }
    }
}

/// The layer's state: the app, for limits and the skin, and the buckets.
#[derive(Clone)]
pub struct Guard {
    pub app: AppState,
    pub limiter: Arc<Limiter>,
}

pub async fn layer(State(guard): State<Guard>, req: Request<Body>, next: Next) -> Response {
    let Some(class) = classify(req.method(), req.uri().path()) else {
        return next.run(req).await;
    };
    // No peer address means no socket, as in tests that call the router directly.
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() else {
        return next.run(req).await;
    };
    let ip = crate::net::client_ip(req.headers(), peer, guard.app.config.trust_proxy);
    let per_minute = class.per_minute(naw_core::limits::install());
    match guard.limiter.check(class, ip, per_minute, Instant::now()) {
        Ok(()) => next.run(req).await,
        Err(Refused { retry_after, first }) => {
            if first {
                tracing::info!(?class, %ip, retry_after, "rate limited");
            }
            let request_id = req.extensions().get::<RequestId>().map(|r| r.0.clone());
            crate::errors::rate_limited(
                &guard.app,
                req.method(),
                req.headers(),
                request_id.as_deref(),
                retry_after,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(raw: &str) -> IpAddr {
        raw.parse().unwrap()
    }

    #[test]
    fn requests_fall_into_their_classes() {
        let get = Method::GET;
        let post = Method::POST;
        assert_eq!(classify(&get, "/filian"), Some(Class::Read));
        assert_eq!(classify(&Method::HEAD, "/"), Some(Class::Read));
        assert_eq!(classify(&get, "/search"), Some(Class::Heavy));
        assert_eq!(classify(&get, "/filian/history"), Some(Class::Heavy));
        assert_eq!(classify(&get, "/filian/diff"), Some(Class::Heavy));
        assert_eq!(classify(&get, "/filian/rev/42"), Some(Class::Heavy));
        assert_eq!(classify(&get, "/media"), Some(Class::Heavy));
        assert_eq!(classify(&get, "/api/complete"), Some(Class::Typing));
        assert_eq!(classify(&post, "/preview"), Some(Class::Typing));
        assert_eq!(classify(&post, "/drafts/save"), Some(Class::Typing));
        assert_eq!(classify(&post, "/filian/edit"), Some(Class::Write));
        assert_eq!(classify(&post, "/login/password"), Some(Class::Write));
        assert_eq!(classify(&post, "/media/upload"), Some(Class::Write));
    }

    #[test]
    fn static_files_and_signed_callers_are_not_counted() {
        let get = Method::GET;
        for path in [
            "/health",
            "/ready",
            "/skin/a/scripts/editor.js",
            "/media/ab/cdef.png",
            "/robots.txt",
        ] {
            assert_eq!(classify(&get, path), None, "{path}");
        }
        assert_eq!(classify(&Method::POST, "/discord/interactions"), None);
        assert_eq!(classify(&Method::OPTIONS, "/filian"), None);
    }

    #[test]
    fn a_burst_passes_and_a_flood_waits() {
        let limiter = Limiter::default();
        let start = Instant::now();
        let who = ip("198.51.100.7");
        for n in 0..30 {
            assert!(
                limiter.check(Class::Write, who, 30, start).is_ok(),
                "request {n}"
            );
        }
        let refused = limiter.check(Class::Write, who, 30, start).unwrap_err();
        assert_eq!(refused.retry_after, 2, "one form refills every two seconds");
        assert!(refused.first, "the first refusal is reported");
        let again = limiter.check(Class::Write, who, 30, start).unwrap_err();
        assert!(!again.first, "a flood is reported once");
        let later = start + Duration::from_secs(2);
        assert!(limiter.check(Class::Write, who, 30, later).is_ok());
        let after_pass = limiter.check(Class::Write, who, 30, later).unwrap_err();
        assert!(
            after_pass.first,
            "a new flood after a pass is reported again"
        );
    }

    #[test]
    fn classes_and_addresses_have_their_own_buckets() {
        let limiter = Limiter::default();
        let now = Instant::now();
        let who = ip("198.51.100.7");
        for _ in 0..10 {
            limiter.check(Class::Heavy, who, 10, now).unwrap();
        }
        assert!(limiter.check(Class::Heavy, who, 10, now).is_err());
        assert!(
            limiter.check(Class::Read, who, 10, now).is_ok(),
            "another class"
        );
        assert!(
            limiter
                .check(Class::Heavy, ip("198.51.100.8"), 10, now)
                .is_ok(),
            "another address"
        );
    }

    #[test]
    fn one_ipv6_network_shares_a_bucket() {
        let limiter = Limiter::default();
        let now = Instant::now();
        limiter
            .check(Class::Write, ip("2001:db8:1:2::1"), 1, now)
            .unwrap();
        assert!(
            limiter
                .check(Class::Write, ip("2001:db8:1:2:ffff::9"), 1, now)
                .is_err(),
            "same /64"
        );
        assert!(
            limiter
                .check(Class::Write, ip("2001:db8:1:3::1"), 1, now)
                .is_ok(),
            "next /64"
        );
        assert_eq!(owner(ip("::ffff:198.51.100.7")), ip("198.51.100.7"));
    }

    #[test]
    fn idle_buckets_are_swept() {
        let limiter = Limiter::default();
        let start = Instant::now();
        for n in 0..SWEEP_AT as u32 {
            let who = IpAddr::V4(std::net::Ipv4Addr::from(n));
            limiter.check(Class::Read, who, 600, start).unwrap();
        }
        limiter
            .check(Class::Read, ip("198.51.100.7"), 600, start + IDLE)
            .unwrap();
        assert_eq!(limiter.buckets.lock().unwrap().len(), 1);
    }
}
