//! Header buttons and footer links from `wikis.settings.chrome`.
//!
//! ```json
//! { "chrome": {
//!     "header": { "new_page": true, "about": true, "languages": true, "theme": true, "search": true, "emotes": true },
//!     "footer": [ { "href": "/about", "label": "About", "lang": "" } ],
//!     "notice": { "en": "A fan project...", "ru": "Фанатский проект..." },
//!     "domains": [ "filian.wiki", "snackers.wiki" ]
//! } }
//! ```
//!
//! Missing or malformed values keep the defaults. A saved footer list
//! replaces the default links, and an empty one means none.

use serde_json::Value;

/// Footer links a wiki may have.
pub const MAX_FOOTER_LINKS: usize = 8;
/// Longest footer notice, in characters.
pub const NOTICE_MAX: usize = 600;
/// Domains a wiki may list as its own.
pub const MAX_DOMAINS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderFlags {
    pub new_page: bool,
    pub about: bool,
    pub languages: bool,
    pub theme: bool,
    pub search: bool,
    pub emotes: bool,
}

impl Default for HeaderFlags {
    fn default() -> Self {
        Self {
            new_page: true,
            about: true,
            languages: true,
            theme: true,
            search: true,
            emotes: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FooterLink {
    pub href: String,
    pub label: String,
    /// Shown only in this interface language; empty for everyone.
    pub lang: String,
}

pub fn header(settings: &Value) -> HeaderFlags {
    let defaults = HeaderFlags::default();
    let Some(h) = settings.get("chrome").and_then(|c| c.get("header")) else {
        return defaults;
    };
    let flag = |key: &str, fallback: bool| h.get(key).and_then(Value::as_bool).unwrap_or(fallback);
    HeaderFlags {
        new_page: flag("new_page", defaults.new_page),
        about: flag("about", defaults.about),
        languages: flag("languages", defaults.languages),
        theme: flag("theme", defaults.theme),
        search: flag("search", defaults.search),
        emotes: flag("emotes", defaults.emotes),
    }
}

/// `None` when the wiki never customised its footer.
pub fn footer(settings: &Value) -> Option<Vec<FooterLink>> {
    let list = settings.get("chrome")?.get("footer")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|item| {
                let href = item.get("href")?.as_str()?.trim().to_string();
                let label = item.get("label")?.as_str()?.trim().to_string();
                let lang = item
                    .get("lang")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                (href_is_safe(&href) && !label.is_empty()).then_some(FooterLink {
                    href,
                    label,
                    lang,
                })
            })
            .take(MAX_FOOTER_LINKS)
            .collect(),
    )
}

/// The footer notice in `lang`, else in the wiki's language, else none. A
/// fan wiki says here that it is not the person it writes about.
pub fn notice(settings: &Value, lang: &str, fallback: &str) -> Option<String> {
    let map = settings.get("chrome")?.get("notice")?;
    [lang, fallback]
        .iter()
        .find_map(|code| {
            map.get(*code)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .map(|text| text.chars().take(NOTICE_MAX).collect())
}

/// Every language's notice, for the admin form.
pub fn notices(settings: &Value) -> serde_json::Map<String, Value> {
    settings
        .get("chrome")
        .and_then(|c| c.get("notice"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// The domains this wiki lists as its own, so a reader can tell it from a copy.
pub fn domains(settings: &Value) -> Vec<String> {
    settings
        .get("chrome")
        .and_then(|c| c.get("domains"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter_map(clean_domain)
                .take(MAX_DOMAINS)
                .collect()
        })
        .unwrap_or_default()
}

/// A bare host name, lowercase: letters, digits, dashes and dots, with a dot.
pub fn clean_domain(raw: &str) -> Option<String> {
    let host = raw
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    let ok = host.len() <= 253
        && host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..")
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.');
    ok.then_some(host)
}

/// A local path or an http(s) URL: never `javascript:`, never `//host`.
pub fn href_is_safe(href: &str) -> bool {
    if href.len() > 500 || href.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    if let Some(rest) = href.strip_prefix('/') {
        return !rest.starts_with('/') && !rest.starts_with('\\');
    }
    let lower = href.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_apply_until_something_is_saved() {
        assert_eq!(header(&json!({})), HeaderFlags::default());
        assert_eq!(footer(&json!({})), None);
        let partial = json!({ "chrome": { "header": { "about": false, "theme": "nope" } } });
        let h = header(&partial);
        assert!(!h.about);
        assert!(h.theme, "a malformed value keeps the default");
        assert_eq!(footer(&json!({ "chrome": { "footer": [] } })), Some(vec![]));
    }

    #[test]
    fn footer_links_are_cleaned() {
        let settings = json!({ "chrome": { "footer": [
            { "href": "/rules", "label": " Rules ", "lang": "EN" },
            { "href": "javascript:alert(1)", "label": "x" },
            { "href": "//evil.example", "label": "x" },
            { "href": "https://discord.gg/snackers", "label": "Discord" },
            { "href": "/empty", "label": "   " },
        ] } });
        let links = footer(&settings).expect("customised");
        assert_eq!(links.len(), 2);
        assert_eq!(
            links[0],
            FooterLink {
                href: "/rules".into(),
                label: "Rules".into(),
                lang: "en".into()
            }
        );
        assert_eq!(links[1].href, "https://discord.gg/snackers");
    }

    #[test]
    fn domains_are_bare_hosts() {
        assert_eq!(
            clean_domain(" https://Filian.Wiki/ "),
            Some("filian.wiki".into())
        );
        assert_eq!(clean_domain("localhost"), None);
        assert_eq!(clean_domain("evil.com/<script>"), None);
        assert_eq!(clean_domain("a..b"), None);
        let settings = serde_json::json!({ "chrome": { "domains": ["filian.wiki", "bad domain", "snackers.wiki"] } });
        assert_eq!(domains(&settings), vec!["filian.wiki", "snackers.wiki"]);
    }

    #[test]
    fn the_notice_falls_back_to_the_wiki_language() {
        let settings =
            serde_json::json!({ "chrome": { "notice": { "en": "Fan project.", "ru": " " } } });
        assert_eq!(
            notice(&settings, "ru", "en").as_deref(),
            Some("Fan project.")
        );
        assert_eq!(notice(&settings, "de", "fr"), None);
    }

    #[test]
    fn only_safe_hrefs_pass() {
        for ok in [
            "/about",
            "/ru/about",
            "https://filian.wiki",
            "mailto:vadim@filian.wiki",
        ] {
            assert!(href_is_safe(ok), "{ok}");
        }
        for bad in [
            "javascript:x",
            "//evil",
            "/\\evil",
            "data:text/html,x",
            "/a b",
            "",
        ] {
            assert!(!href_is_safe(bad), "{bad}");
        }
    }
}
