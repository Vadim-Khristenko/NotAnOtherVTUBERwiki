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
    /// The language of the article this request is about. The wiki's own
    /// language unless the address names another one (`/ru/about` or
    /// `ru.wiki.example`).
    pub content_locale: String,
    /// How `content_locale` was chosen, which decides how links are built.
    pub locale_via: LocaleVia,
    /// The routed path, without a language prefix: what a language switch
    /// keeps when it moves the reader to another language.
    pub path: String,
    /// The largest image one upload may be, so the editor can say so before
    /// sending a file that would be refused.
    pub upload_max_bytes: usize,
}

/// Where the article language of a request came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocaleVia {
    /// No language in the address: the wiki's own language.
    Default,
    /// A path prefix, `/ru/about`.
    Path,
    /// A language subdomain, `ru.wiki.example/about`.
    Subdomain,
}

/// How a wiki spells a language in its links, from
/// `settings.languages.urls`: `path` (the default, works on any host) or
/// `subdomain` (needs DNS and a certificate for every language).
pub fn language_urls_by_subdomain(settings: &serde_json::Value) -> bool {
    settings
        .get("languages")
        .and_then(|v| v.get("urls"))
        .and_then(|v| v.as_str())
        == Some("subdomain")
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

    /// A local path in this request's article language: `/about` becomes
    /// `/ru/about` on a Russian page reached by prefix. The wiki's own
    /// language and subdomain addresses need no prefix.
    pub fn link(&self, path: &str) -> String {
        if self.locale_via == LocaleVia::Path && self.content_locale != self.wiki.default_locale {
            if path == "/" {
                format!("/{}", self.content_locale)
            } else {
                format!("/{}{path}", self.content_locale)
            }
        } else {
            path.to_string()
        }
    }

    /// The prefix `link` adds, for templates: "" or "/ru".
    pub fn base(&self) -> String {
        if self.locale_via == LocaleVia::Path && self.content_locale != self.wiki.default_locale {
            format!("/{}", self.content_locale)
        } else {
            String::new()
        }
    }

    /// `path` in another language, the way this wiki spells languages in its
    /// addresses. A subdomain wiki gets an absolute URL, because the language
    /// is in the host.
    pub fn link_for(&self, locale: &str, path: &str) -> String {
        let default = locale == self.wiki.default_locale;
        if language_urls_by_subdomain(&self.wiki.settings)
            && let Some(domain) = self.wiki.domain.as_deref()
        {
            return if default {
                format!("https://{domain}{path}")
            } else {
                format!("https://{locale}.{domain}{path}")
            };
        }
        if default {
            path.to_string()
        } else if path == "/" {
            format!("/{locale}")
        } else {
            format!("/{locale}{path}")
        }
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
        let header = crate::chrome::header(&self.wiki.settings);
        // A saved footer replaces the translated defaults. Local links keep
        // the reader's article language; a link meant for one interface
        // language shows only there.
        let footer_links: Option<Vec<minijinja::Value>> =
            crate::chrome::footer(&self.wiki.settings).map(|links| {
                links
                    .into_iter()
                    .filter(|l| l.lang.is_empty() || l.lang == self.lang)
                    .map(|l| {
                        let href = if l.href.starts_with('/') {
                            self.link(&l.href)
                        } else {
                            l.href
                        };
                        minijinja::context! { href => href, label => l.label }
                    })
                    .collect()
            });
        // Code plus the language's name in itself, so the switcher can say
        // "Русский" to the person looking for it rather than "RU".
        let language_options: Vec<minijinja::Value> = offered
            .iter()
            .map(|code| {
                let native = messages
                    .meta(code)
                    .map(|m| m.native_name.clone())
                    .unwrap_or_else(|| code.to_uppercase());
                // Choosing a language moves the reader to this article in
                // it, and `?lang=` makes the interface follow. An option is
                // only "where you are" when both already match: an English
                // article under a Russian interface still offers Russian.
                let href = format!("{}?lang={code}", self.link_for(code, &self.path));
                minijinja::context! {
                    code => code.clone(),
                    native_name => native,
                    current => *code == self.lang && *code == self.content_locale,
                    href => href,
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
            content_lang => self.content_locale.clone(),
            show_new_page => header.new_page,
            show_about => header.about,
            show_languages => header.languages,
            show_theme => header.theme,
            show_search => header.search,
            footer_links => footer_links,
            lang_native => messages
                .meta(&self.lang)
                .map(|m| m.native_name.clone())
                .unwrap_or_else(|| self.lang.to_uppercase()),
            base => self.base(),
            // Shadows the install-wide global of the same name, so a template
            // offers exactly what this wiki offers.
            languages => offered,
            language_options => language_options,
            signed_in => self.actor.is_signed_in(),
            username => self.actor.username.clone(),
            // The name to show; the username when no display name is set.
            display_name => self.actor.display_name.clone().or_else(|| self.actor.username.clone()),
            avatar_url => self.actor.avatar_url.clone(),
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
    let skin_now = state.skin.current();
    let known = |code: &str| skin_now.messages.has(code);
    // A language subdomain first (`ru.wiki.example`), so it is not swallowed
    // by the default wiki fallback in `resolve_wiki`.
    let (wiki, host_locale) = match language_subdomain(request_host(headers), &wikis, &known) {
        Some((wiki, locale)) => (wiki, Some(locale)),
        None => match resolve_wiki(request_host(headers), &wikis) {
            Some(wiki) => (wiki, None),
            None => return Ok(None),
        },
    };
    let wiki = wiki.clone();
    let actor = crate::perm::resolve(&state.db, wiki.id, &wiki.settings, user).await?;
    let skin = state.skin.current();
    let disabled = disabled_languages(&wiki.settings);
    let offered = |code: &str| {
        let code = code.trim().to_ascii_lowercase();
        skin.messages.has(&code) && !disabled.contains(&code)
    };
    let path_locale = headers
        .get(crate::locale_path::LOCALE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // A language this wiki switched off is not reachable by address either.
    let (content_locale, locale_via) = match (path_locale, host_locale) {
        (Some(locale), _) if offered(&locale) => (locale, LocaleVia::Path),
        (_, Some(locale)) if offered(&locale) => (locale, LocaleVia::Subdomain),
        _ => (wiki.default_locale.clone(), LocaleVia::Default),
    };
    // An address that names a language is a deliberate choice, the way a
    // language subdomain is on Wikipedia: the interface follows it. Without
    // one, the reader's own preference decides.
    let lang = if locale_via == LocaleVia::Default {
        pick_language(&offered, headers, user, &wiki.default_locale)
    } else {
        content_locale.clone()
    };
    let path = headers
        .get(crate::locale_path::PATH_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/")
        .to_string();
    Ok(Some(Ctx {
        wiki,
        actor,
        lang,
        skin,
        content_locale,
        locale_via,
        path,
        upload_max_bytes: state.config.upload_max_bytes,
    }))
}

/// `ru.wiki.example` for a wiki on `wiki.example`: the wiki plus the
/// language, when the first label is an installed language and the rest is
/// exactly a wiki's domain. Never falls back to the default wiki; that is
/// `resolve_wiki`'s job once this finds nothing.
pub fn language_subdomain<'a>(
    host: Option<&str>,
    wikis: &'a [WikiRef],
    known: &dyn Fn(&str) -> bool,
) -> Option<(&'a WikiRef, String)> {
    let host = host?.split(':').next()?.trim().to_lowercase();
    let (label, rest) = host.split_once('.')?;
    if !known(label) {
        return None;
    }
    // A wiki that really lives on `ru.example` keeps it.
    if wikis
        .iter()
        .any(|w| w.domain.as_deref().map(str::to_lowercase).as_deref() == Some(host.as_str()))
    {
        return None;
    }
    wikis
        .iter()
        .find(|w| w.domain.as_deref().map(str::to_lowercase).as_deref() == Some(rest))
        .map(|w| (w, label.to_string()))
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
    fn a_language_subdomain_names_the_wiki_and_the_language() {
        let known = |code: &str| matches!(code, "en" | "ru");
        let list = vec![
            wiki("filian", Some("filian.wiki"), true),
            wiki("russian-only", Some("ru.example.test"), false),
        ];
        let (hit, lang) =
            language_subdomain(Some("ru.filian.wiki:443"), &list, &known).expect("hit");
        assert_eq!(hit.slug, "filian");
        assert_eq!(lang, "ru");
        assert!(language_subdomain(Some("filian.wiki"), &list, &known).is_none());
        assert!(
            language_subdomain(Some("de.filian.wiki"), &list, &known).is_none(),
            "no German pack"
        );
        assert!(language_subdomain(Some("www.filian.wiki"), &list, &known).is_none());
        // A wiki that really lives on a ru. host keeps it.
        assert!(language_subdomain(Some("ru.example.test"), &list, &known).is_none());
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
