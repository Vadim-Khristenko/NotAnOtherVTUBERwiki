//! `/ru/about` is the Russian article at `about`.
//!
//! A leading segment naming an installed language is stripped before
//! routing, so every route works in every language. The layer wraps the
//! whole router because axum middleware runs after routing. The result
//! travels in headers this layer owns; client copies are removed first.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request, Uri};
use axum::middleware::Next;
use axum::response::Response;

use naw_core::state::AppState;

/// The content language from the path prefix.
pub const LOCALE_HEADER: &str = "x-naw-content-locale";
/// The path as routed, without a language prefix.
pub const PATH_HEADER: &str = "x-naw-path";

/// A language code shape (`ru`, `pt-br`, `zh-hant`), checked before the lookup.
fn looks_like_language(segment: &str) -> bool {
    let mut parts = segment.split('-');
    let Some(primary) = parts.next() else {
        return false;
    };
    (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `/ru/about/edit` into `("ru", "/about/edit")` when `ru` is installed.
fn split(path: &str, known: &dyn Fn(&str) -> bool) -> Option<(String, String)> {
    let rest = path.strip_prefix('/')?;
    let (first, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !looks_like_language(first) || !known(first) {
        return None;
    }
    let routed = if tail.is_empty() || tail == "/" {
        "/"
    } else {
        tail
    };
    Some((first.to_string(), routed.to_string()))
}

pub async fn layer(State(app): State<AppState>, mut req: Request<Body>, next: Next) -> Response {
    req.headers_mut().remove(LOCALE_HEADER);
    req.headers_mut().remove(PATH_HEADER);

    let skin = app.skin.current();
    let known = |code: &str| skin.messages.has(code);
    let uri = req.uri().clone();
    let (locale, routed_path) = match split(uri.path(), &known) {
        Some((locale, rest)) => (Some(locale), rest),
        None => (None, uri.path().to_string()),
    };

    if let Some(locale) = &locale {
        let routed = match uri.query() {
            Some(q) => format!("{routed_path}?{q}"),
            None => routed_path.clone(),
        };
        if let Ok(new_uri) = routed.parse::<Uri>() {
            *req.uri_mut() = new_uri;
        }
        if let Ok(value) = HeaderValue::from_str(locale) {
            req.headers_mut().insert(LOCALE_HEADER, value);
        }
    }
    if let Ok(value) = HeaderValue::from_str(&routed_path) {
        req.headers_mut().insert(PATH_HEADER, value);
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(code: &str) -> bool {
        matches!(code, "en" | "ru" | "pt-br")
    }

    #[test]
    fn a_language_prefix_is_split_off() {
        assert_eq!(
            split("/ru/about", &known),
            Some(("ru".into(), "/about".into()))
        );
        assert_eq!(
            split("/ru/about/edit", &known),
            Some(("ru".into(), "/about/edit".into()))
        );
        assert_eq!(split("/ru", &known), Some(("ru".into(), "/".into())));
        assert_eq!(split("/ru/", &known), Some(("ru".into(), "/".into())));
        assert_eq!(
            split("/pt-br/x", &known),
            Some(("pt-br".into(), "/x".into()))
        );
    }

    #[test]
    fn anything_else_is_left_alone() {
        assert_eq!(split("/about", &known), None);
        assert_eq!(split("/de/about", &known), None, "no German pack");
        assert_eq!(split("/rubric", &known), None);
        assert_eq!(split("/", &known), None);
        assert_eq!(split("/RU/about", &known), None, "codes are lowercase");
        assert_eq!(split("/admin", &known), None);
    }
}
