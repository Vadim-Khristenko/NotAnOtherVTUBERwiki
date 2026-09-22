//! `?lang=` on any URL.
//!
//! Switching language used to mean choosing in a dropdown and then finding a
//! small "Go" next to it. People changed the dropdown, nothing happened, and the
//! feature read as broken. It was: a control that needs a second click on a
//! different control is not a switch.
//!
//! Now any URL takes `?lang=ru`. The middleware stores the choice, strips the
//! parameter and redirects, so the address bar stays clean, the choice survives
//! the next click, and a link with a language in it can be shared. MediaWiki
//! spells this `uselang`.
//!
//! **Why a GET may change something here.** Everything else that changes state
//! in this engine is a POST, because the session cookie is `SameSite=Lax` and
//! that is the CSRF defence. This is the one exemption, and it is narrow: the
//! only thing it writes is a display preference, it grants nothing, and the
//! worst a hostile link can do is show somebody the interface in Norwegian.
//! Copy this exemption nowhere else.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use naw_core::state::AppState;

/// The query parameter, and the name of the cookie it fills.
pub const PARAM: &str = "lang";

/// Splits a query string into the language it asks for and everything else.
///
/// Returns `None` when there is no `lang` parameter, which is the common case
/// and must cost nothing. The remaining query is rebuilt verbatim: other
/// parameters belong to the page and have to survive the redirect, or switching
/// language on a search results page would throw the search away.
fn take_lang(query: &str) -> Option<(String, String)> {
    let mut found: Option<String> = None;
    let mut rest: Vec<&str> = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            // Last one wins, matching how a server reads repeated parameters.
            Some((PARAM, value)) => found = Some(value.to_string()),
            _ if pair == PARAM => found = Some(String::new()),
            _ => rest.push(pair),
        }
    }
    found.map(|lang| (lang, rest.join("&")))
}

/// Percent-decodes a query value. Only what a language tag can contain: the
/// result is checked against the catalogue immediately afterwards, so anything
/// this mangles simply fails that check.
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

/// Builds the cookie that remembers the choice for a year.
pub fn cookie_for(lang: &str) -> String {
    cookie::Cookie::build((crate::resolve::LANG_COOKIE, lang.to_ascii_lowercase()))
        .path("/")
        // Readable by script on purpose: it is a display preference, and a skin
        // may want to reflect it without a round trip. Nothing is authorised
        // from it, so there is nothing to steal.
        .http_only(false)
        .same_site(cookie::SameSite::Lax)
        .max_age(cookie::time::Duration::days(365))
        .build()
        .to_string()
}

pub async fn layer(State(app): State<AppState>, req: Request<Body>, next: Next) -> Response {
    // Only a navigation. Redirecting a POST would discard its body, and a form
    // that carries a language parameter by accident must still submit.
    if req.method() != Method::GET {
        return next.run(req).await;
    }
    let Some((raw, rest)) = req.uri().query().and_then(take_lang) else {
        return next.run(req).await;
    };
    let chosen = decode(&raw).to_ascii_lowercase();
    // An unknown language is dropped rather than stored. Storing it would leave
    // a cookie that is ignored on every later request, which looks exactly like
    // a broken switcher.
    if !app.skin.current().messages.has(&chosen) {
        return next.run(req).await;
    }

    let path = req.uri().path();
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
        // A path that cannot be a header value cannot have been routed to
        // either, so serving the page is the safe answer.
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
        // A parameter that merely starts with the name is not the name.
        assert_eq!(take_lang("language=ru"), None);
        assert_eq!(take_lang("langx=ru"), None);
    }

    #[test]
    fn the_language_is_taken_out_and_the_rest_is_kept() {
        // The point of keeping the rest: switching language on a search results
        // page must not throw the search away.
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
        // Neither is a language, and both have to reach the `has` check rather
        // than panic or be silently treated as something else.
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
        // Not HttpOnly: it authorises nothing and a skin may want to read it.
        assert!(!cookie.contains("HttpOnly"));
    }
}
