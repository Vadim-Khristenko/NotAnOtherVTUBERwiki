//! `?lang=` on any URL: the middleware stores the choice in a cookie, strips
//! the parameter and redirects.
//!
//! The one GET that writes state. It is safe despite `SameSite=Lax` because
//! it only sets a display preference and grants nothing. Do not copy it.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use naw_core::state::AppState;

/// The query parameter and cookie name.
pub const PARAM: &str = "lang";

/// Splits a query into the requested language and the rest, rebuilt verbatim
/// so other parameters survive the redirect. `None` without `lang`.
fn take_lang(query: &str) -> Option<(String, String)> {
    let mut found: Option<String> = None;
    let mut rest: Vec<&str> = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            // The last one wins, as servers read repeated parameters.
            Some((PARAM, value)) => found = Some(value.to_string()),
            _ if pair == PARAM => found = Some(String::new()),
            _ => rest.push(pair),
        }
    }
    found.map(|lang| (lang, rest.join("&")))
}

/// Percent-decodes a query value; the result is checked against the catalogue.
fn decode(value: &str) -> String {
    let bytes = value.replace('+', " ");
    let bytes = bytes.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// The cookie that remembers the choice for a year.
pub fn cookie_for(lang: &str) -> String {
    cookie::Cookie::build((crate::resolve::LANG_COOKIE, lang.to_ascii_lowercase()))
        .path("/")
        // Readable by script: a display preference that authorises nothing.
        .http_only(false)
        .same_site(cookie::SameSite::Lax)
        .max_age(cookie::time::Duration::days(365))
        .build()
        .to_string()
}

/// Expires the language cookie, so the account setting decides again.
pub fn clear_cookie() -> String {
    cookie::Cookie::build((crate::resolve::LANG_COOKIE, ""))
        .path("/")
        .http_only(false)
        .same_site(cookie::SameSite::Lax)
        .max_age(cookie::time::Duration::ZERO)
        .build()
        .to_string()
}

pub async fn layer(State(app): State<AppState>, req: Request<Body>, next: Next) -> Response {
    // Redirecting a POST would discard its body.
    if req.method() != Method::GET {
        return next.run(req).await;
    }
    let Some((raw, rest)) = req.uri().query().and_then(take_lang) else {
        return next.run(req).await;
    };
    let chosen = decode(&raw).to_ascii_lowercase();
    // An unknown language is dropped rather than stored.
    if !app.skin.current().messages.has(&chosen) {
        return next.run(req).await;
    }

    // Land back on the address asked for, language prefix included.
    let routed = req.uri().path();
    let path = match req
        .headers()
        .get(crate::locale_path::LOCALE_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some(locale) if routed == "/" => format!("/{locale}"),
        Some(locale) => format!("/{locale}{routed}"),
        None => routed.to_string(),
    };
    let target = if rest.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{rest}")
    };

    let mut response = match axum::http::HeaderValue::from_str(&target) {
        Ok(location) => Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(header::LOCATION, location)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::SEE_OTHER.into_response()),
        Err(_) => return next.run(req).await,
    };
    if let Ok(value) = axum::http::HeaderValue::from_str(&cookie_for(&chosen)) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_without_a_language_is_left_alone() {
        assert_eq!(take_lang(""), None);
        assert_eq!(take_lang("q=filian"), None);
        assert_eq!(take_lang("page=2&sort=new"), None);
        assert_eq!(take_lang("language=ru"), None);
        assert_eq!(take_lang("langx=ru"), None);
    }

    #[test]
    fn the_language_is_taken_out_and_the_rest_is_kept() {
        assert_eq!(
            take_lang("q=filian&lang=ru"),
            Some(("ru".to_string(), "q=filian".to_string()))
        );
        assert_eq!(
            take_lang("lang=ru&q=filian&page=2"),
            Some(("ru".to_string(), "q=filian&page=2".to_string()))
        );
        assert_eq!(
            take_lang("lang=ru"),
            Some(("ru".to_string(), String::new()))
        );
    }

    #[test]
    fn a_repeated_parameter_takes_the_last_value() {
        assert_eq!(
            take_lang("lang=en&lang=ru"),
            Some(("ru".to_string(), String::new()))
        );
    }

    #[test]
    fn an_empty_or_encoded_value_survives_to_the_catalogue_check() {
        assert_eq!(take_lang("lang="), Some((String::new(), String::new())));
        assert_eq!(take_lang("lang"), Some((String::new(), String::new())));
        assert_eq!(decode("ru%2DRU"), "ru-RU");
        assert_eq!(decode("ru+RU"), "ru RU");
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("%"), "%");
        assert_eq!(decode("ru"), "ru");
    }

    #[test]
    fn the_cookie_is_scoped_to_the_whole_site_and_lasts() {
        let cookie = cookie_for("RU");
        assert!(cookie.starts_with("naw_lang=ru"), "{cookie}");
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Max-Age=31536000"));
        assert!(!cookie.contains("HttpOnly"));
    }
}
