//! "Not for all ages": pages in this category open behind a warning, as a
//! mature stream does on Twitch. They stay safe for work; the warning is for
//! topics that depend on age and context. The reader's "continue" is kept in
//! a cookie for a month, so the warning shows once, and the server draws it,
//! so it works without a script.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use naw_markdown::categories::Membership;

/// Category keys that mark a page: `[[Category:Not For All Ages]]` or `[[Category:NFAA]]`.
pub(crate) const KEYS: &[&str] = &["not-for-all-ages", "nfaa"];

const COOKIE: &str = "naw_nfaa";
/// How long a "continue" lasts.
const MAX_AGE: u32 = 30 * 24 * 3600;

pub(crate) fn marked(categories: &[Membership]) -> bool {
    categories.iter().any(|c| KEYS.contains(&c.key.as_str()))
}

/// The reader has said "continue" before.
pub(crate) fn accepted(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|pair| pair.trim() == format!("{COOKIE}=1"))
}

/// "Continue": remember it and go back to the page.
pub(crate) fn accept(location: &str) -> Response {
    let mut res = StatusCode::SEE_OTHER.into_response();
    if let Ok(to) = HeaderValue::from_str(location) {
        res.headers_mut().insert(header::LOCATION, to);
    }
    if let Ok(cookie) = HeaderValue::from_str(&format!(
        "{COOKIE}=1; Max-Age={MAX_AGE}; Path=/; SameSite=Lax"
    )) {
        res.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(key: &str) -> Membership {
        Membership {
            key: key.to_string(),
            name: key.to_string(),
            sort: None,
        }
    }

    #[test]
    fn a_page_is_marked_by_its_category() {
        assert!(marked(&[member("vod"), member("not-for-all-ages")]));
        assert!(marked(&[member("nfaa")]));
        assert!(!marked(&[member("vod"), member("filian")]));
        assert!(!marked(&[]));
    }

    #[test]
    fn only_the_exact_cookie_counts_as_a_continue() {
        let mut h = HeaderMap::new();
        assert!(!accepted(&h));
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; naw_nfaa=1"),
        );
        assert!(accepted(&h));
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("naw_nfaa=0; xnaw_nfaa=1"),
        );
        assert!(!accepted(&h));
    }

    #[test]
    fn continuing_sets_the_cookie_and_goes_back() {
        let res = accept("/nfaa-vods");
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        assert_eq!(res.headers()[header::LOCATION], "/nfaa-vods");
        let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(
            cookie.starts_with("naw_nfaa=1;") && cookie.contains("Path=/"),
            "{cookie}"
        );
    }
}
