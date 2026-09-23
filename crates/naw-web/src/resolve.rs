//! Wiki resolver: Host header, then path default, then 404.
//!
//! One resolver for every hosting shape. Per-wiki differences live in
//! `wikis.settings`, never in resolver branches.

use axum::http::{HeaderMap, header};
use serde_json::Value;
use uuid::Uuid;

use naw_core::error::AppError;

/// The wiki fields the resolver needs. Built from one `wikis` row.
///
/// `Clone` so a handler can own its wiki across an await instead of holding a
/// borrow into the loaded list. One clone per request buys the whole pile of
/// lifetime gymnastics back, and the strings are small.
#[derive(Clone)]
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
pub fn request_host(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::HOST)?.to_str().ok()
}

/// Loads every wiki row the resolver can choose from.
pub async fn load_wikis(db: &sqlx::PgPool) -> Result<Vec<WikiRef>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT id, slug, domain, name, default_locale, settings FROM wikis ORDER BY slug"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let is_default = row
                .settings
                .get("default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            WikiRef {
                id: row.id,
                slug: row.slug,
                domain: row.domain,
                name: row.name,
                default_locale: row.default_locale,
                settings: row.settings,
                is_default,
            }
        })
        .collect())
}

/// Fallback brand for a request that matches no wiki at all.
pub const UNKNOWN_WIKI: &str = "NotAnotherWiki";

/// One request's wiki plus its authority over that wiki.
///
/// Every handler past the resolver needs both, and resolving them separately
/// was how `/new` and `/{slug}/edit` ended up open to the internet: the wiki
/// lookup was mandatory and the permission check simply was not there. Bundling
/// them means a handler cannot reach a page without having also resolved who is
/// asking.
pub struct Ctx {
    pub wiki: WikiRef,
    pub actor: crate::perm::Actor,
    /// Language for the interface: labels, buttons, dates.
    ///
    /// Separate from `wiki.default_locale`, which is the language of the
    /// *content* and which drives the search stemmer. A Russian reader on an
    /// English wiki gets Russian chrome around English articles, and conflating
    /// the two would force them to pick one.
    pub lang: String,
    /// The templates and messages this request renders with. One snapshot per
    /// request, so a reload landing mid-request cannot mix two versions of the
    /// skin into one page. An `Arc` clone, which is a pointer bump.
    pub skin: std::sync::Arc<naw_core::skin::Loaded>,
}

impl Ctx {
    /// One message in this request's language.
    ///
    /// Handlers build a handful of strings that templates cannot: page titles
    /// that embed an article name, and the summary a revert writes into the
    /// history. Those are as visible as anything in a template and have to
    /// follow the same language.
    pub fn t(&self, key: &str) -> String {
        self.skin.messages.render(&self.lang, key, &[])
    }

    /// One message with interpolations, as `t("k", &[("name", "value")])`.
    pub fn t_with(&self, key: &str, args: &[(&str, &str)]) -> String {
        let owned: Vec<(String, String)> = args
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        self.skin.messages.render(&self.lang, key, &owned)
    }

    /// The languages this wiki offers, in catalogue order.
    pub fn offered_languages(&self) -> Vec<String> {
        let disabled = disabled_languages(&self.wiki.settings);
        self.skin
            .messages
            .languages()
            .into_iter()
            .filter(|code| !disabled.iter().any(|d| d == code))
            .map(str::to_string)
            .collect()
    }
}

/// Cookie that remembers an explicit language choice.
pub const LANG_COOKIE: &str = "naw_lang";

/// Languages a wiki has switched off, from `wikis.settings.languages.disabled`.
///
/// A pack is installed for the whole install; whether a particular wiki offers
/// it is that wiki's call. A community that has not reviewed the Russian
/// translation of its own terminology can hide it without touching the files
/// every other wiki uses. Malformed settings read as "nothing disabled", which
/// fails open towards showing a language rather than towards hiding all of them.
pub fn disabled_languages(settings: &serde_json::Value) -> Vec<String> {
    settings
        .get("languages")
        .and_then(|v| v.get("disabled"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Reads the language cookie, if it names a language this wiki offers.
///
/// Parsed by hand from the Cookie header for the same reason the session id is:
/// one header, two known names, no dependency.
fn cookie_language(headers: &HeaderMap, offered: &dyn Fn(&str) -> bool) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let mut parts = pair.trim().splitn(2, '=');
        if parts.next() == Some(LANG_COOKIE) {
            let value = parts.next()?.trim();
            // Never trust it: the value lands in a lang attribute and drives a
            // catalogue lookup, and it arrives from the client.
            if offered(value) {
                return Some(value.to_ascii_lowercase());
            }
            return None;
        }
    }
    None
}

/// Chooses the interface language for one request.
///
/// In order:
///
/// 1. An explicit choice, from the language cookie. The most recent deliberate
///    act wins, and it is the only step an anonymous reader can reach.
/// 2. The account's stored preference.
/// 3. `Accept-Language`.
/// 4. The wiki's own language.
///
/// Each step only counts if this wiki offers that language, so a preference for
/// something nobody has translated, or something this wiki has switched off,
/// falls through rather than rendering a page of message keys.
///
/// The cookie sits above the account preference on purpose. Without it, every
/// signed-in account was pinned to `users.locale`, which defaults to `en` and
/// which nothing could change: the interface was translated and unreachable at
/// the same time.
fn pick_language(
    offered: &dyn Fn(&str) -> bool,
    headers: &HeaderMap,
    user: Option<&crate::auth::session::CurrentUser>,
    wiki_locale: &str,
) -> String {
    if let Some(chosen) = cookie_language(headers, offered) {
        return chosen;
    }
    if let Some(preferred) = user.map(|u| u.locale.as_str())
        && offered(preferred)
    {
        return preferred.to_string();
    }
    if let Some(accepted) = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| naw_core::i18n::negotiate_with(raw, offered))
    {
        return accepted;
    }
    wiki_locale.to_string()
}

impl Ctx {
    /// The template bindings the shared chrome needs: who is signed in, and
    /// which navigation the skin may show them. A skin must never render an
    /// Edit link that the server will refuse, so the flags come from the same
    /// `Actor` the handler gates on.
    pub fn chrome_context(&self) -> minijinja::Value {
        use crate::perm::Capability;
        let messages = &self.skin.messages;
        let offered = self.offered_languages();
        // Code plus the language's name in itself, so the switcher can say
        // "Русский" to the person looking for it rather than "RU".
        let language_options: Vec<minijinja::Value> = offered
            .iter()
            .map(|code| {
                let native = messages
                    .meta(code)
                    .map(|m| m.native_name.clone())
                    .unwrap_or_else(|| code.to_uppercase());
                minijinja::context! {
                    code => code.clone(),
                    native_name => native,
                    current => *code == self.lang,
                }
            })
            .collect();
        let dir = messages
            .meta(&self.lang)
            .map(|m| if m.is_rtl() { "rtl" } else { "ltr" })
            .unwrap_or("ltr");
        minijinja::context! {
            wiki_name => self.wiki.name.clone(),
            // `lang` is what t() reads, so it is the interface language.
            lang => self.lang.clone(),
            dir => dir,
            // The content language, for a lang attribute on the article itself
            // when it differs from the chrome.
            content_lang => self.wiki.default_locale.clone(),
            // Shadows the install-wide global of the same name, so a template
            // offers exactly what this wiki offers.
            languages => offered,
            language_options => language_options,
            signed_in => self.actor.is_signed_in(),
            username => self.actor.username.clone(),
            // The name to show; the username when no display name is set.
            display_name => self.actor.display_name.clone().or_else(|| self.actor.username.clone()),
            can_create => self.actor.can(Capability::PageCreate),
            can_edit => self.actor.can(Capability::PageEdit),
            can_moderate => self.actor.can(Capability::PageDelete),
            can_admin => self.actor.can(Capability::AdminPanel),
        }
    }
}

/// Resolves the wiki from the Host header and the actor from the session that
/// `session::layer` already loaded. `None` means no wiki matched, which the
/// caller turns into a 404.
pub async fn context(
    state: &naw_core::state::AppState,
    headers: &HeaderMap,
    user: Option<&crate::auth::session::CurrentUser>,
) -> Result<Option<Ctx>, AppError> {
    let wikis = load_wikis(&state.db).await?;
    let Some(wiki) = resolve_wiki(request_host(headers), &wikis) else {
        return Ok(None);
    };
    let wiki = wiki.clone();
    let actor = crate::perm::resolve(&state.db, wiki.id, &wiki.settings, user).await?;
    let skin = state.skin.current();
    let disabled = disabled_languages(&wiki.settings);
    let offered = |code: &str| {
        let code = code.trim().to_ascii_lowercase();
        skin.messages.has(&code) && !disabled.contains(&code)
    };
    let lang = pick_language(&offered, headers, user, &wiki.default_locale);
    Ok(Some(Ctx {
        wiki,
        actor,
        lang,
        skin,
    }))
}

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
