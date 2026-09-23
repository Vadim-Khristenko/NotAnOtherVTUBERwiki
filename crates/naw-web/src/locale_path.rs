//! `/ru/about` is the Russian article at `about`.
//!
//! One wiki hosts every language of an article under one slug. The language is
//! the first path segment when it names an installed language pack, and the
//! rest of the path is routed as if the prefix were not there, so every route
//! (`/{slug}`, `/{slug}/edit`, `/{slug}/history` and the rest) works in every
//! language without being declared twice.
//!
//! The layer wraps the whole router, not a route, because it changes which
//! route matches: axum's own middleware runs after routing, too late to rewrite
//! the path it routes on.
//!
//! What it found travels to the handlers as request headers it owns. Any copy a
//! client sent is removed first, so nobody can pick a language prefix, or a
//! path, by typing a header.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request, Uri};
use axum::middleware::Next;
use axum::response::Response;

use naw_core::state::AppState;

/// The content language taken from the path prefix, when there was one.
pub const LOCALE_HEADER: &str = "x-naw-content-locale";
/// The path as it will be routed, without any language prefix.
pub const PATH_HEADER: &str = "x-naw-path";

/// A language code shape: `ru`, `pt-br`, `zh-hant`. Checked before the pack
/// lookup so an arbitrary first segment never reaches the catalogue.
fn looks_like_language(segment: &str) -> bool {
    let mut parts = segment.split('-');
    let Some(primary) = parts.next() else {
        return false;
    };
    (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Splits `/ru/about/edit` into `("ru", "/about/edit")` when `ru` is a language
/// this install has a pack for. `/ru` alone is the Russian home page.
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
