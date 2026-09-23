//! Header buttons and footer links from `wikis.settings.chrome`.
//!
//! ```json
//! { "chrome": {
//!     "header": { "new_page": true, "about": true, "languages": true, "theme": true, "search": true },
//!     "footer": [ { "href": "/about", "label": "About", "lang": "" } ]
//! } }
//! ```
//!
//! Missing or malformed values keep the defaults. A saved footer list
//! replaces the default links, and an empty one means none.

use serde_json::Value;

/// Footer links a wiki may have.
pub const MAX_FOOTER_LINKS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderFlags {
    pub new_page: bool,
    pub about: bool,
    pub languages: bool,
    pub theme: bool,
    pub search: bool,
}

impl Default for HeaderFlags {
    fn default() -> Self {
        Self {
            new_page: true,
            about: true,
            languages: true,
            theme: true,
            search: true,
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
