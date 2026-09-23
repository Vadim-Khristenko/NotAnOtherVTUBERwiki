//! Content-Security-Policy and framing headers on every response.
//!
//! Scripts run only from this origin or with the response's nonce, nothing
//! may frame the wiki, no plugins, forms submit here only. Inline styles
//! stay allowed for skin theming.

use axum::body::Body;
use axum::http::{HeaderValue, Request, header};
use axum::middleware::Next;
use axum::response::Response;

pub async fn layer(req: Request<Body>, next: Next) -> Response {
    let nonce = crate::auth::random_token();
    let policy = policy(&nonce);
    let mut response = naw_core::csp::scope(nonce, next.run(req)).await;
    // A 304 updates the browser's stored headers; a new nonce would block the
    // scripts of the cached copy.
    let not_modified = response.status() == axum::http::StatusCode::NOT_MODIFIED;
    let headers = response.headers_mut();
    if !not_modified && let Ok(value) = HeaderValue::from_str(&policy) {
        headers
            .entry(header::CONTENT_SECURITY_POLICY)
            .or_insert(value);
    }
    // For browsers without `frame-ancestors`.
    headers
        .entry(header::X_FRAME_OPTIONS)
        .or_insert(HeaderValue::from_static("DENY"));
    headers
        .entry(header::REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
    headers
        .entry(header::HeaderName::from_static(
            "cross-origin-opener-policy",
        ))
        .or_insert(HeaderValue::from_static("same-origin"));
    response
}

/// Images only from this origin: articles show local uploads, avatars and
/// emote copies.
fn policy(nonce: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self' 'nonce-{nonce}'; \
         style-src 'self' 'unsafe-inline'; img-src 'self' data:; object-src 'none'; \
         base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_need_the_nonce_and_nothing_may_frame_the_page() {
        let policy = policy("n0nce");
        assert!(
            policy.contains("script-src 'self' 'nonce-n0nce';"),
            "{policy}"
        );
        assert!(
            !policy.contains("script-src 'self' 'unsafe-inline'"),
            "{policy}"
        );
        assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
        assert!(policy.contains("object-src 'none'"), "{policy}");
        assert!(policy.contains("img-src 'self' data:;"), "{policy}");
    }

    #[test]
    fn every_inline_script_in_every_skin_carries_the_nonce() {
        // A blocked script fails silently in the browser.
        let skins = format!("{}/../../skins", env!("CARGO_MANIFEST_DIR"));
        let mut checked = 0;
        let mut stack = vec![std::path::PathBuf::from(skins)];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("skins dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "html") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("template");
                for (at, _) in source.match_indices("<script") {
                    let tag =
                        &source[at..source[at..].find('>').map_or(source.len(), |end| at + end)];
                    assert!(
                        tag.contains("nonce=\"{{ csp_nonce() }}\""),
                        "{}: {tag}",
                        path.display()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no scripts found, wrong directory?");
    }
}
