//! Refuses state-changing requests that another site started.
//!
//! `SameSite=Lax` keeps the session cookie off cross-site POSTs, but not a
//! cookie a cross-site POST *sets*: a form elsewhere could sign a reader into
//! somebody else's account. Every request but GET, HEAD and OPTIONS must
//! therefore come from this origin, judged by `Sec-Fetch-Site` when the
//! browser sends it and by `Origin` otherwise. A request with neither is not
//! from a browser and carries no ambient credentials, so it passes.

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::errors::{Kind, MarkExt};

pub async fn layer(req: Request<Body>, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        || same_origin(req.headers())
    {
        return next.run(req).await;
    }
    tracing::info!(
        method = %req.method(),
        path = req.uri().path(),
        "cross-origin request refused"
    );
    (StatusCode::FORBIDDEN, "cross-origin request refused")
        .into_response()
        .marked(Kind::Forbidden)
}

fn same_origin(headers: &HeaderMap) -> bool {
    let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(site) = value("sec-fetch-site") {
        // `none` is the reader acting directly, such as a bookmark.
        return matches!(site, "same-origin" | "none");
    }
    let Some(origin) = value(header::ORIGIN.as_str()) else {
        return true;
    };
    let Some(host) = value(header::HOST.as_str()) else {
        return false;
    };
    origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .is_some_and(|origin_host| origin_host.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn fetch_metadata_decides_when_present() {
        assert!(same_origin(&headers(&[("sec-fetch-site", "same-origin")])));
        assert!(same_origin(&headers(&[("sec-fetch-site", "none")])));
        assert!(!same_origin(&headers(&[("sec-fetch-site", "cross-site")])));
        // A sibling subdomain is still another origin.
        assert!(!same_origin(&headers(&[("sec-fetch-site", "same-site")])));
    }

    #[test]
    fn origin_must_match_the_host() {
        let host = ("host", "alpha.filian.wiki");
        assert!(same_origin(&headers(&[
            host,
            ("origin", "https://alpha.filian.wiki")
        ])));
        assert!(!same_origin(&headers(&[
            host,
            ("origin", "https://evil.example")
        ])));
        assert!(!same_origin(&headers(&[
            host,
            ("origin", "https://alpha.filian.wiki.evil.example")
        ])));
        assert!(!same_origin(&headers(&[host, ("origin", "null")])));
        assert!(!same_origin(&headers(&[(
            "origin",
            "https://alpha.filian.wiki"
        )])));
    }

    #[test]
    fn a_request_without_browser_headers_passes() {
        assert!(same_origin(&headers(&[("host", "alpha.filian.wiki")])));
    }
}
