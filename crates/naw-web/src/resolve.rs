//! Wiki resolver: Host header, then path default, then 404.
//!
//! One resolver for every hosting shape. Per-wiki differences live in
//! `wikis.settings`, never in resolver branches.

use serde_json::Value;
use uuid::Uuid;

/// The wiki fields the resolver needs. Built from one `wikis` row.
pub struct WikiRef {
    pub id: Uuid,
    // Read by the edit slice (page CRUD addresses wikis by slug). Allowed until then.
    #[allow(dead_code)]
    pub slug: String,
    pub domain: Option<String>,
    pub name: String,
    pub default_locale: String,
    pub settings: Value,
    pub is_default: bool,
}

/// Picks the wiki for a request. `host` is the raw Host header value and may
/// carry a port, which is stripped. Exact domain wins, then aliases from
/// `settings.aliases`, then the wiki flagged default. Returns `None` when
/// nothing matches, which the caller turns into a 404.
///
/// Limitation, documented on purpose: IPv6 literal hosts are not parsed,
/// only `name` and `name:port` shapes. Production hosts are DNS names.
pub fn resolve_wiki<'a>(host: Option<&str>, wikis: &'a [WikiRef]) -> Option<&'a WikiRef> {
    let bare = host
        .map(|h| {
            h.split(':')
                .next()
                .unwrap_or_default()
                .trim()
                .to_lowercase()
        })
        .filter(|h| !h.is_empty());
    if let Some(name) = bare.as_deref() {
        if let Some(hit) = wikis
            .iter()
            .find(|w| w.domain.as_deref().map(|d| d.to_lowercase()) == Some(name.to_string()))
        {
            return Some(hit);
        }
        if let Some(hit) = wikis.iter().find(|w| {
            w.settings
                .get("aliases")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .any(|a| a.to_lowercase() == name)
                })
                .unwrap_or(false)
        }) {
            return Some(hit);
        }
    }
    wikis.iter().find(|w| w.is_default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wiki(slug: &str, domain: Option<&str>, default: bool) -> WikiRef {
        WikiRef {
            id: Uuid::new_v4(),
            slug: slug.to_string(),
            domain: domain.map(|d| d.to_string()),
            name: slug.to_string(),
            default_locale: "en".to_string(),
            settings: json!({"default": default}),
            is_default: default,
        }
    }

    #[test]
    fn exact_domain_wins_over_default() {
        let list = vec![wiki("snackers", Some("snackers.vai-rice.space"), true)];
        let hit = resolve_wiki(Some("snackers.vai-rice.space"), &list).unwrap();
        assert_eq!(hit.slug, "snackers");
    }

    #[test]
    fn port_is_stripped() {
        let list = vec![wiki("snackers", Some("snackers.vai-rice.space"), true)];
        let hit = resolve_wiki(Some("snackers.vai-rice.space:8080"), &list).unwrap();
        assert_eq!(hit.slug, "snackers");
    }

    #[test]
    fn alias_matches() {
        let mut w = wiki("snackers", Some("snackers.vai-rice.space"), true);
        w.settings = json!({"aliases": ["filian.wiki"]});
        let list = vec![w];
        let hit = resolve_wiki(Some("filian.wiki"), &list).unwrap();
        assert_eq!(hit.slug, "snackers");
    }

    #[test]
    fn unknown_host_falls_back_to_default() {
        let list = vec![wiki("snackers", Some("snackers.vai-rice.space"), true)];
        let hit = resolve_wiki(Some("unknown.example"), &list).unwrap();
        assert_eq!(hit.slug, "snackers");
    }

    #[test]
    fn no_host_and_no_default_is_404() {
        let list = vec![wiki("plain", None, false)];
        assert!(resolve_wiki(None, &list).is_none());
    }
}
