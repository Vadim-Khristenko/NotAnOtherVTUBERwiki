//! Per-request ids and the detail `NAW_LOG_TRACE=1` turns on.
//!
//! Every response carries `x-request-id`, the string that ties a report to
//! the log. Trace mode logs headers through `logging::header_is_loggable`.

use std::time::Instant;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

use naw_core::logging;

/// Echoed on every response.
pub const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// 12 hex characters: short enough to read out, enough to find a request.
fn short_id() -> String {
    Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// Whether a client-supplied id is safe to echo and log: a narrow charset,
/// so it cannot forge a header or a log line.
fn id_is_sane(value: &str) -> bool {
    (8..=64).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Reuses a sane client id so a proxy can correlate; otherwise a new one.
fn incoming_id(req: &Request<Body>) -> Option<String> {
    let raw = req.headers().get(&REQUEST_ID)?.to_str().ok()?;
    let trimmed = raw.trim();
    id_is_sane(trimmed).then(|| trimmed.to_string())
}

fn log_headers(req: &Request<Body>, id: &str) {
    for (name, value) in req.headers() {
        let name = name.as_str();
        if !logging::header_is_loggable(name) {
            tracing::trace!(request_id = id, header = name, value = "[redacted]");
            continue;
        }
        match value.to_str() {
            Ok(text) => tracing::trace!(request_id = id, header = name, value = text),
            Err(_) => tracing::trace!(request_id = id, header = name, value = "[not utf8]"),
        }
    }
}

/// Assigns an id, times the request and attaches the id to the response.
pub async fn layer(mut req: Request<Body>, next: Next) -> Response {
    let settings = logging::settings_from_env();
    let id = incoming_id(&req).unwrap_or_else(short_id);
    // Handed down so an error page can print the id before the header exists.
    req.extensions_mut()
        .insert(crate::errors::RequestId(id.clone()));
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    if settings.trace {
        let query = req.uri().query().unwrap_or_default().to_string();
        tracing::debug!(
            request_id = %id,
            %method,
            path = %path,
            query = %query,
            version = ?req.version(),
            "request in"
        );
        log_headers(&req, &id);
    }

    let started = Instant::now();
    let mut response = next.run(req).await;
    let elapsed_ms = started.elapsed().as_millis();
    let status = response.status().as_u16();

    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID, value);
    }

    if status >= 500 {
        tracing::error!(request_id = %id, %method, path = %path, status, elapsed_ms, "request failed");
    } else if settings.trace {
        tracing::debug!(request_id = %id, %method, path = %path, status, elapsed_ms, "request out");
        if let Some(kind) = response.headers().get(header::CONTENT_TYPE) {
            tracing::trace!(request_id = %id, content_type = ?kind);
        }
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with(id: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/");
        if let Some(value) = id {
            builder = builder.header(REQUEST_ID, value);
        }
        builder.body(Body::empty()).expect("request")
    }

    #[test]
    fn a_generated_id_is_short_and_hex() {
        let id = short_id();
        assert_eq!(id.len(), 12);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, short_id());
    }

    #[test]
    fn a_sane_client_id_is_reused_for_correlation() {
        let req = request_with(Some("deploy-check-01"));
        assert_eq!(incoming_id(&req).as_deref(), Some("deploy-check-01"));
    }

    #[test]
    fn a_hostile_client_id_is_refused() {
        // http refuses CR and LF in a header, but the validator must hold anyway.
        for bad in [
            "short",
            "with space",
            "new\nline",
            "semi;colon",
            "quote\"mark",
            "<script>",
            "carriage\rreturn",
            "",
            &"x".repeat(65),
        ] {
            assert!(!id_is_sane(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn a_rejected_header_value_falls_back_to_a_generated_id() {
        for bad in ["short", "with space", "semi;colon"] {
            let req = request_with(Some(bad));
            assert_eq!(incoming_id(&req), None, "{bad:?}");
        }
    }

    #[test]
    fn a_missing_header_means_we_generate_one() {
        assert_eq!(incoming_id(&request_with(None)), None);
    }

    #[test]
    fn the_header_name_is_the_conventional_one() {
        assert_eq!(REQUEST_ID.as_str(), "x-request-id");
    }
}
