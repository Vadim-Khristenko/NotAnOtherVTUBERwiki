//! Interface translation from language packs in `locales/<lang>/`.
//!
//! Templates call `t("area.key")` and `tn("area.key", n)`; the language comes
//! from `lang` in the render context. Nothing here fails a render: a missing
//! message falls back to the fallback language, then to the key itself.

use std::collections::BTreeMap;
use std::sync::Arc;

use minijinja::value::{Kwargs, Value};
use minijinja::{Environment, Error, State};

use crate::error::AppError;

/// Used when a message is missing from the requested language.
pub const FALLBACK: &str = "en";

/// CLDR plural categories used by the supported languages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plural {
    One,
    Few,
    Many,
    Other,
}

impl Plural {
    /// The key suffix, as in `revisions.one`.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }
}

/// The plural form `n` takes in `lang`.
///
/// Slavic languages pick by the last one and two digits (1, 21, 101 alike;
/// 2 to 4; 11 to 14 excepted). Unlisted languages get the English rule.
pub fn plural_of(lang: &str, n: u64) -> Plural {
    let base = lang
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match base.as_str() {
        "ru" | "uk" | "be" => {
            let ten = n % 10;
            let hundred = n % 100;
            if ten == 1 && hundred != 11 {
                Plural::One
            } else if (2..=4).contains(&ten) && !(12..=14).contains(&hundred) {
                Plural::Few
            } else {
                Plural::Many
            }
        }
        // One form for every count.
        "ja" | "ko" | "zh" | "tr" | "id" | "th" | "vi" => Plural::Other,
        _ => {
            if n == 1 {
                Plural::One
            } else {
                Plural::Other
            }
        }
    }
}

/// Somebody credited for a language pack.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Author {
    pub name: String,
    /// Free text such as `maintainer` or `translator`.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// `metadata.toml` of a language pack.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Meta {
    pub code: String,
    /// English name, for the admin panel.
    pub name: String,
    /// Name in the language itself, for the switcher.
    pub native_name: String,
    pub version: String,
    /// Engine release the pack was last checked against.
    #[serde(default)]
    pub engine: Option<String>,
    /// A disabled pack stays loaded for the admin panel but is never offered.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// `ltr` or `rtl`.
    #[serde(default = "default_direction")]
    pub direction: String,
    #[serde(default)]
    pub authors: Vec<Author>,
}

fn default_enabled() -> bool {
    true
}

fn default_direction() -> String {
    "ltr".to_string()
}

impl Meta {
    /// Metadata for a pack built by `from_pairs`.
    fn bare(code: &str) -> Self {
        Self {
            code: code.to_string(),
            name: code.to_string(),
            native_name: code.to_string(),
            version: "0".to_string(),
            engine: None,
            enabled: true,
            direction: default_direction(),
            authors: Vec::new(),
        }
    }

    pub fn is_rtl(&self) -> bool {
        self.direction.eq_ignore_ascii_case("rtl")
    }
}

/// One pack as the admin panel shows it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PackReport {
    pub meta: Meta,
    pub messages: usize,
    /// Keys the fallback language has and this pack lacks.
    pub missing: usize,
    /// Completeness, rounded down.
    pub percent: u8,
}

/// Every message by language and key, plus each pack's metadata.
#[derive(Debug, Default)]
pub struct Catalog {
    languages: BTreeMap<String, BTreeMap<String, String>>,
    meta: BTreeMap<String, Meta>,
    /// Why a pack was skipped, for the admin panel.
    problems: Vec<String>,
}

impl Catalog {
    /// Loads every pack under `dir`.
    ///
    /// A pack is a directory with `metadata.toml`; every other `*.toml` in it is
    /// one area whose file name is the key prefix (`ru/search.toml` supplies
    /// `search.*`). Broken packs are logged, recorded in `problems` and skipped,
    /// so they never stop the engine.
    pub fn load(dir: &str) -> Result<Self, AppError> {
        let mut catalog = Self::default();
        let Ok(entries) = std::fs::read_dir(dir) else {
            tracing::warn!(
                dir,
                "no locales directory, the interface will show message keys"
            );
            return Ok(catalog);
        };
        let mut packs: Vec<std::path::PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && path.join("metadata.toml").is_file())
            .collect();
        packs.sort();
        for pack in packs {
            if let Err(problem) = catalog.load_pack(&pack) {
                tracing::warn!(pack = %pack.display(), problem = %problem, "language pack skipped");
                catalog.problems.push(problem);
            }
        }
        Ok(catalog)
    }

    fn load_pack(&mut self, dir: &std::path::Path) -> Result<(), String> {
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| format!("{}: directory name is not text", dir.display()))?;
        let meta_path = dir.join("metadata.toml");
        let raw = std::fs::read_to_string(&meta_path)
            .map_err(|err| format!("{}: {err}", meta_path.display()))?;
        let meta: Meta =
            toml::from_str(&raw).map_err(|err| format!("{}: {err}", meta_path.display()))?;
        // The directory name is what `?lang=`, the cookie and Accept-Language match.
        if meta.code.to_ascii_lowercase() != dir_name {
            return Err(format!(
                "{}: code is {:?} but the directory is {:?}",
                meta_path.display(),
                meta.code,
                dir_name
            ));
        }

        let mut areas: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map_err(|err| format!("{}: {err}", dir.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "toml")
                    && path.file_stem().is_some_and(|stem| stem != "metadata")
            })
            .collect();
        areas.sort();

        let mut flat = BTreeMap::new();
        for area_path in areas {
            let Some(area) = area_path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let raw = std::fs::read_to_string(&area_path)
                .map_err(|err| format!("{}: {err}", area_path.display()))?;
            let parsed: toml::Value =
                toml::from_str(&raw).map_err(|err| format!("{}: {err}", area_path.display()))?;
            flatten(&parsed, area.to_string(), &mut flat);
        }
        tracing::debug!(lang = %dir_name, messages = flat.len(), "language pack loaded");
        self.languages.insert(dir_name.clone(), flat);
        self.meta.insert(dir_name, meta);
        Ok(())
    }

    /// Builds a catalogue from pairs, for tests.
    pub fn from_pairs(lang: &str, pairs: &[(&str, &str)]) -> Self {
        let mut catalog = Self::default();
        let code = lang.to_ascii_lowercase();
        catalog.languages.insert(
            code.clone(),
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        catalog.meta.insert(code.clone(), Meta::bare(&code));
        catalog
    }

    /// Enabled languages, sorted.
    pub fn languages(&self) -> Vec<&str> {
        self.languages
            .keys()
            .filter(|code| self.meta.get(*code).is_none_or(|m| m.enabled))
            .map(String::as_str)
            .collect()
    }

    /// Whether a language may be chosen; false for disabled packs.
    pub fn has(&self, lang: &str) -> bool {
        let code = normalize_lang(lang);
        self.languages.contains_key(&code) && self.meta.get(&code).is_none_or(|m| m.enabled)
    }

    pub fn meta(&self, lang: &str) -> Option<&Meta> {
        self.meta.get(&normalize_lang(lang))
    }

    /// Packs that failed to load, with the reason.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Every loaded pack with its completeness against the fallback language.
    pub fn report(&self) -> Vec<PackReport> {
        let reference: Option<&BTreeMap<String, String>> = self.languages.get(FALLBACK);
        self.languages
            .iter()
            .map(|(code, messages)| {
                let (missing, percent) = match reference {
                    Some(reference) if code != FALLBACK && !reference.is_empty() => {
                        let missing = reference
                            .keys()
                            .filter(|key| !covers(messages, key))
                            .count();
                        let have = reference.len() - missing;
                        (missing, ((have * 100) / reference.len()) as u8)
                    }
                    _ => (0, 100),
                };
                PackReport {
                    meta: self
                        .meta
                        .get(code)
                        .cloned()
                        .unwrap_or_else(|| Meta::bare(code)),
                    messages: messages.len(),
                    missing,
                    percent,
                }
            })
            .collect()
    }
}

/// Whether a pack supplies a reference key. A plural key counts as covered
/// when the pack has any form of it, since languages use different forms.
fn covers(messages: &BTreeMap<String, String>, key: &str) -> bool {
    if messages.contains_key(key) {
        return true;
    }
    let Some((stem, form)) = key.rsplit_once('.') else {
        return false;
    };
    if !matches!(form, "one" | "few" | "many" | "other") {
        return false;
    }
    ["one", "few", "many", "other"]
        .iter()
        .any(|form| messages.contains_key(&format!("{stem}.{form}")))
}

impl Catalog {
    /// One message: the exact language, its base (`ru-RU` finds `ru`), then the
    /// fallback language.
    pub fn lookup(&self, lang: &str, key: &str) -> Option<&str> {
        let normalized = normalize_lang(lang);
        if let Some(hit) = self.direct(&normalized, key) {
            return Some(hit);
        }
        if let Some(base) = normalized.split('-').next()
            && base != normalized
            && let Some(hit) = self.direct(base, key)
        {
            return Some(hit);
        }
        if normalized != FALLBACK {
            return self.direct(FALLBACK, key);
        }
        None
    }

    fn direct(&self, lang: &str, key: &str) -> Option<&str> {
        self.languages.get(lang)?.get(key).map(String::as_str)
    }

    /// A message with its arguments, or the key when there is none.
    pub fn render(&self, lang: &str, key: &str, args: &[(String, String)]) -> String {
        let template = self.lookup(lang, key).unwrap_or(key);
        interpolate(template, args)
    }

    /// The plural form of `key` for `n`: `key.<form>`, then `key.other`, then `key`.
    pub fn render_plural(
        &self,
        lang: &str,
        key: &str,
        n: u64,
        args: &[(String, String)],
    ) -> String {
        let form = plural_of(lang, n);
        let mut all = args.to_vec();
        all.push(("n".to_string(), n.to_string()));
        for candidate in [
            format!("{key}.{}", form.suffix()),
            format!("{key}.other"),
            key.to_string(),
        ] {
            if let Some(template) = self.lookup(lang, &candidate) {
                return interpolate(template, &all);
            }
        }
        interpolate(key, &all)
    }
}

fn normalize_lang(lang: &str) -> String {
    lang.trim().to_ascii_lowercase().replace('_', "-")
}

/// Picks an interface language from `Accept-Language`, in the browser's order
/// of preference; a regional tag matches its base language. `None` when
/// nothing matches or the header is malformed.
pub fn negotiate(header: &str, catalog: &Catalog) -> Option<String> {
    negotiate_with(header, &|code: &str| catalog.has(code))
}

/// As [`negotiate`], against any predicate, such as a wiki's enabled languages.
pub fn negotiate_with(header: &str, available: &dyn Fn(&str) -> bool) -> Option<String> {
    let mut candidates: Vec<(f32, usize, String)> = Vec::new();
    for (position, part) in header.split(',').enumerate() {
        let mut pieces = part.split(';');
        let Some(tag) = pieces.next().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        if tag == "*" {
            continue;
        }
        // An unparseable q counts as absent, not as zero.
        let quality = pieces
            .find_map(|piece| piece.trim().strip_prefix("q="))
            .and_then(|value| value.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        if quality <= 0.0 {
            continue;
        }
        // The position breaks ties between equal qualities.
        candidates.push((quality, position, normalize_lang(tag)));
    }
    candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    for (_, _, tag) in candidates {
        if available(&tag) {
            return Some(tag);
        }
        if let Some(base) = tag.split('-').next()
            && base != tag
            && available(base)
        {
            return Some(base.to_string());
        }
    }
    None
}

/// Replaces `{name}` with the matching argument, escaped. An unknown
/// placeholder stays as written so the mismatch is visible. The message text
/// is trusted, the arguments are not.
fn interpolate(template: &str, args: &[(String, String)]) -> String {
    if !template.contains('{') {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(k, _)| k == name) {
                    Some((_, value)) => out.push_str(&crate::html::escape(value)),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                // Unbalanced brace: emit the rest verbatim.
                out.push('{');
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Flattens nested TOML tables into dotted keys.
fn flatten(value: &toml::Value, prefix: String, out: &mut BTreeMap<String, String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, nested) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(nested, path, out);
            }
        }
        toml::Value::String(text) => {
            out.insert(prefix, text.clone());
        }
        other => {
            out.insert(prefix, other.to_string());
        }
    }
}

/// The render language from the template context.
fn lang_of(state: &State) -> String {
    state
        .lookup("lang")
        .as_ref()
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| FALLBACK.to_string())
}

/// MiniJinja keyword arguments as pairs; `assert_all_used` catches a typo.
fn pairs_from(kwargs: &Kwargs) -> Result<Vec<(String, String)>, Error> {
    let mut pairs = Vec::new();
    for key in kwargs.args() {
        let value: Value = kwargs.get(key)?;
        pairs.push((key.to_string(), value.to_string()));
    }
    Ok(pairs)
}

/// Registers `t` and `tn` on a template environment.
///
/// `t("key", name=value)` is one message; `tn("key", n, ...)` a counted one
/// with `{n}` always available. Both return values already marked safe:
/// messages may carry markup, and `interpolate` escapes every argument, so a
/// skin never needs `| safe` and cannot get it wrong.
pub fn install(env: &mut Environment<'static>, catalog: Arc<Catalog>) {
    let for_t = Arc::clone(&catalog);
    env.add_function(
        "t",
        move |state: &State, key: &str, kwargs: Kwargs| -> Result<Value, Error> {
            let pairs = pairs_from(&kwargs)?;
            kwargs.assert_all_used()?;
            Ok(Value::from_safe_string(for_t.render(
                &lang_of(state),
                key,
                &pairs,
            )))
        },
    );

    let for_tn = Arc::clone(&catalog);
    env.add_function(
        "tn",
        move |state: &State, key: &str, n: i64, kwargs: Kwargs| -> Result<Value, Error> {
            let pairs = pairs_from(&kwargs)?;
            kwargs.assert_all_used()?;
            // A count below zero is an upstream bug; clamp rather than panic on the cast.
            let n = n.max(0) as u64;
            Ok(Value::from_safe_string(for_tn.render_plural(
                &lang_of(state),
                key,
                n,
                &pairs,
            )))
        },
    );

    let names: Vec<String> = catalog
        .languages()
        .into_iter()
        .map(str::to_string)
        .collect();
    env.add_global("languages", Value::from(names));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let mut c = Catalog::default();
        c.languages.insert(
            "en".into(),
            [
                ("search.heading", "Search"),
                ("history.revisions.one", "{n} revision"),
                ("history.revisions.other", "{n} revisions"),
                ("greet", "Hello {name}"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        c.languages.insert(
            "ru".into(),
            [
                ("search.heading", "Поиск"),
                ("history.revisions.one", "{n} правка"),
                ("history.revisions.few", "{n} правки"),
                ("history.revisions.many", "{n} правок"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        c
    }

    #[test]
    fn a_message_comes_back_in_the_asked_language() {
        let c = catalog();
        assert_eq!(c.render("en", "search.heading", &[]), "Search");
        assert_eq!(c.render("ru", "search.heading", &[]), "Поиск");
    }

    #[test]
    fn a_region_suffix_finds_the_base_language() {
        let c = catalog();
        assert_eq!(c.render("ru-RU", "search.heading", &[]), "Поиск");
        assert_eq!(c.render("ru_RU", "search.heading", &[]), "Поиск");
        assert_eq!(c.render("RU-ru", "search.heading", &[]), "Поиск");
    }

    #[test]
    fn an_untranslated_language_falls_back_to_english() {
        let c = catalog();
        assert_eq!(c.render("de", "search.heading", &[]), "Search");
        assert_eq!(c.render("ja-JP", "search.heading", &[]), "Search");
    }

    #[test]
    fn a_missing_key_shows_the_key_and_never_fails() {
        let c = catalog();
        assert_eq!(c.render("en", "search.nope", &[]), "search.nope");
        assert_eq!(c.render("ru", "totally.absent", &[]), "totally.absent");
        assert_eq!(Catalog::default().render("en", "any.key", &[]), "any.key");
    }

    #[test]
    fn a_key_missing_from_russian_but_present_in_english_falls_through() {
        let c = catalog();
        assert_eq!(
            c.render("ru", "greet", &[("name".into(), "VAI".into())]),
            "Hello VAI"
        );
    }

    #[test]
    fn placeholders_are_filled_from_the_arguments() {
        let c = catalog();
        assert_eq!(
            c.render("en", "greet", &[("name".into(), "Filian".into())]),
            "Hello Filian"
        );
    }

    #[test]
    fn an_argument_cannot_inject_markup_into_a_message_that_carries_markup() {
        // The query is visitor input, and t() returns markup marked safe.
        let c = Catalog::from_pairs(
            "en",
            &[(
                "search.nothing",
                "Nothing matched <strong>{query}</strong>.",
            )],
        );
        let rendered = c.render(
            "en",
            "search.nothing",
            &[("query".into(), "<img src=x onerror=alert(1)>".into())],
        );
        assert!(
            rendered.contains("&lt;img src=x onerror=alert(1)&gt;"),
            "{rendered}"
        );
        assert!(rendered.contains("<strong>"), "{rendered}");
        assert!(!rendered.contains("<img"), "{rendered}");
    }

    #[test]
    fn every_dangerous_character_in_an_argument_is_escaped() {
        assert_eq!(crate::html::escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#39;");
        assert_eq!(crate::html::escape("Филиан 🍪"), "Филиан 🍪");
        assert_eq!(crate::html::escape(""), "");
    }

    #[test]
    fn an_unknown_placeholder_stays_visible_rather_than_vanishing() {
        assert_eq!(interpolate("Hello {name}", &[]), "Hello {name}");
        assert_eq!(
            interpolate("{a} and {b}", &[("a".into(), "one".into())]),
            "one and {b}"
        );
    }

    #[test]
    fn an_unbalanced_brace_does_not_lose_the_rest_of_the_message() {
        assert_eq!(interpolate("50% of {", &[]), "50% of {");
        assert_eq!(interpolate("a {b", &[]), "a {b");
        assert_eq!(interpolate("no braces", &[]), "no braces");
        assert_eq!(interpolate("", &[]), "");
    }

    #[test]
    fn english_takes_two_plural_forms() {
        assert_eq!(plural_of("en", 0), Plural::Other);
        assert_eq!(plural_of("en", 1), Plural::One);
        assert_eq!(plural_of("en", 2), Plural::Other);
        assert_eq!(plural_of("en", 21), Plural::Other);
    }

    #[test]
    fn russian_takes_three_and_the_teens_are_the_exception() {
        for one in [1, 21, 31, 101, 1001] {
            assert_eq!(plural_of("ru", one), Plural::One, "{one}");
        }
        for few in [2, 3, 4, 22, 23, 24, 102] {
            assert_eq!(plural_of("ru", few), Plural::Few, "{few}");
        }
        for many in [0, 5, 6, 10, 11, 12, 13, 14, 15, 25, 111, 112] {
            assert_eq!(plural_of("ru", many), Plural::Many, "{many}");
        }
    }

    #[test]
    fn a_language_without_plurals_gets_one_form() {
        for n in [0, 1, 2, 5, 11, 21] {
            assert_eq!(plural_of("ja", n), Plural::Other, "{n}");
            assert_eq!(plural_of("zh-Hans", n), Plural::Other, "{n}");
            assert_eq!(plural_of("tr", n), Plural::Other, "{n}");
        }
    }

    #[test]
    fn counted_messages_pick_the_right_form_and_get_n_for_free() {
        let c = catalog();
        assert_eq!(
            c.render_plural("en", "history.revisions", 1, &[]),
            "1 revision"
        );
        assert_eq!(
            c.render_plural("en", "history.revisions", 5, &[]),
            "5 revisions"
        );
        assert_eq!(
            c.render_plural("ru", "history.revisions", 1, &[]),
            "1 правка"
        );
        assert_eq!(
            c.render_plural("ru", "history.revisions", 3, &[]),
            "3 правки"
        );
        assert_eq!(
            c.render_plural("ru", "history.revisions", 5, &[]),
            "5 правок"
        );
        assert_eq!(
            c.render_plural("ru", "history.revisions", 11, &[]),
            "11 правок"
        );
        assert_eq!(
            c.render_plural("ru", "history.revisions", 21, &[]),
            "21 правка"
        );
    }

    #[test]
    fn a_half_translated_plural_still_reads_as_words() {
        let partial = Catalog::from_pairs(
            "ru",
            &[
                ("history.revisions.one", "{n} правка"),
                ("history.revisions.other", "{n} правок"),
            ],
        );
        assert_eq!(
            partial.render_plural("ru", "history.revisions", 3, &[]),
            "3 правок"
        );
    }

    #[test]
    fn nested_tables_flatten_into_dotted_keys() {
        let parsed: toml::Value = toml::from_str(
            r#"
            [search]
            heading = "Search"
            [search.hint]
            body = "Try fewer words"
            [history]
            title = "History"
            "#,
        )
        .expect("valid toml");
        let mut flat = BTreeMap::new();
        flatten(&parsed, String::new(), &mut flat);
        assert_eq!(
            flat.get("search.heading").map(String::as_str),
            Some("Search")
        );
        assert_eq!(
            flat.get("search.hint.body").map(String::as_str),
            Some("Try fewer words")
        );
        assert_eq!(
            flat.get("history.title").map(String::as_str),
            Some("History")
        );
    }

    #[test]
    fn a_browser_header_picks_a_language_we_have() {
        let c = catalog();
        assert_eq!(
            negotiate("ru-RU,ru;q=0.9,en-US;q=0.8,en;q=0.7", &c).as_deref(),
            Some("ru")
        );
        assert_eq!(negotiate("en-GB,en;q=0.9", &c).as_deref(), Some("en"));
        assert_eq!(negotiate("ru", &c).as_deref(), Some("ru"));
    }

    #[test]
    fn quality_beats_header_order() {
        let c = catalog();
        assert_eq!(negotiate("de;q=0.5,ru;q=0.9", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate("ru;q=0.2,en;q=0.8", &c).as_deref(), Some("en"));
        assert_eq!(negotiate("ru,en", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate("en,ru", &c).as_deref(), Some("en"));
    }

    #[test]
    fn an_explicit_refusal_is_honoured() {
        let c = catalog();
        assert_eq!(negotiate("ru;q=0,en", &c).as_deref(), Some("en"));
    }

    #[test]
    fn a_header_with_nothing_we_speak_defers_to_the_wiki() {
        let c = catalog();
        for header in ["de,fr;q=0.8", "*", "", "   ", ";;;", "q=1", ",,,"] {
            assert_eq!(negotiate(header, &c), None, "{header:?}");
        }
    }

    #[test]
    fn junk_in_the_header_does_not_break_negotiation() {
        let c = catalog();
        assert_eq!(negotiate("ru;q=not-a-number", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate("ru;q=", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate("  RU-ru  ;  q=0.9  ", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate(&"x".repeat(5000), &c), None);
    }

    /// Writes a throwaway locales tree, one directory per call.
    fn scratch_tree(name: &str, packs: &[(&str, &[(&str, &str)])]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("naw-locales-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (dir, files) in packs {
            let pack = root.join(dir);
            std::fs::create_dir_all(&pack).expect("pack dir");
            for (file, body) in *files {
                std::fs::write(pack.join(file), body).expect("pack file");
            }
        }
        root
    }

    const EN_META: &str = r#"
        code = "en"
        name = "English"
        native_name = "English"
        version = "1.0.0"
    "#;

    #[test]
    fn a_pack_directory_loads_with_the_file_name_as_the_prefix() {
        let root = scratch_tree(
            "prefix",
            &[(
                "en",
                &[
                    ("metadata.toml", EN_META),
                    (
                        "search.toml",
                        "heading = \"Search\"\n[results]\none = \"{n} result\"\n",
                    ),
                    ("nav.toml", "sign_in = \"Sign in\"\n"),
                ],
            )],
        );
        let c = Catalog::load(root.to_str().expect("utf8")).expect("loads");
        assert_eq!(c.render("en", "search.heading", &[]), "Search");
        assert_eq!(c.render("en", "nav.sign_in", &[]), "Sign in");
        assert_eq!(c.render_plural("en", "search.results", 1, &[]), "1 result");
        assert_eq!(c.render("en", "metadata.code", &[]), "metadata.code");
        assert_eq!(c.meta("en").map(|m| m.version.as_str()), Some("1.0.0"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_directory_without_metadata_is_not_a_pack() {
        let root = scratch_tree("nometa", &[("en", &[("nav.toml", "sign_in = \"x\"\n")])]);
        let c = Catalog::load(root.to_str().expect("utf8")).expect("loads");
        assert!(c.languages().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_code_that_disagrees_with_its_directory_is_refused_and_reported() {
        let root = scratch_tree(
            "mismatch",
            &[(
                "russian",
                &[(
                    "metadata.toml",
                    "code = \"ru\"\nname = \"Russian\"\nnative_name = \"Русский\"\nversion = \"1\"\n",
                )],
            )],
        );
        let c = Catalog::load(root.to_str().expect("utf8")).expect("loads");
        assert!(c.languages().is_empty());
        assert_eq!(c.problems().len(), 1);
        assert!(c.problems()[0].contains("directory"), "{:?}", c.problems());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_broken_file_skips_its_pack_and_spares_the_others() {
        let root = scratch_tree(
            "broken",
            &[
                (
                    "en",
                    &[
                        ("metadata.toml", EN_META),
                        ("nav.toml", "sign_in = \"Sign in\"\n"),
                    ],
                ),
                (
                    "ru",
                    &[
                        (
                            "metadata.toml",
                            "code = \"ru\"\nname = \"Russian\"\nnative_name = \"Русский\"\nversion = \"1\"\n",
                        ),
                        ("nav.toml", "sign_in = \"Войти\n"),
                    ],
                ),
            ],
        );
        let c = Catalog::load(root.to_str().expect("utf8")).expect("loads");
        assert_eq!(c.languages(), vec!["en"]);
        assert_eq!(c.problems().len(), 1);
        assert!(c.problems()[0].contains("nav.toml"), "{:?}", c.problems());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_disabled_pack_is_loaded_but_never_offered() {
        let root = scratch_tree(
            "disabled",
            &[
                (
                    "en",
                    &[
                        ("metadata.toml", EN_META),
                        ("nav.toml", "sign_in = \"Sign in\"\n"),
                    ],
                ),
                (
                    "ru",
                    &[
                        (
                            "metadata.toml",
                            "code = \"ru\"\nname = \"Russian\"\nnative_name = \"Русский\"\nversion = \"1\"\nenabled = false\n",
                        ),
                        ("nav.toml", "sign_in = \"Войти\"\n"),
                    ],
                ),
            ],
        );
        let c = Catalog::load(root.to_str().expect("utf8")).expect("loads");
        assert_eq!(c.languages(), vec!["en"]);
        assert!(!c.has("ru"));
        assert!(!c.has("ru-RU"));
        let report = c.report();
        assert!(
            report
                .iter()
                .any(|r| r.meta.code == "ru" && !r.meta.enabled)
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn completeness_counts_a_russian_plural_as_covered() {
        let mut c = Catalog::default();
        c.languages.insert(
            "en".into(),
            [
                ("history.revisions.one", "{n} revision"),
                ("history.revisions.other", "{n} revisions"),
                ("nav.sign_in", "Sign in"),
                ("nav.sign_out", "Sign out"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        c.languages.insert(
            "ru".into(),
            [
                ("history.revisions.one", "{n} версия"),
                ("history.revisions.few", "{n} версии"),
                ("history.revisions.many", "{n} версий"),
                ("nav.sign_in", "Войти"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        let report = c.report();
        let ru = report.iter().find(|r| r.meta.code == "ru").expect("ru");
        assert_eq!(ru.missing, 1);
        assert_eq!(ru.percent, 75);
        let en = report.iter().find(|r| r.meta.code == "en").expect("en");
        assert_eq!((en.missing, en.percent), (0, 100));
    }

    #[test]
    fn the_shipped_packs_load_cleanly_and_russian_is_complete() {
        let dir = format!("{}/../../locales", env!("CARGO_MANIFEST_DIR"));
        let c = Catalog::load(&dir).expect("loads");
        assert!(c.problems().is_empty(), "{:?}", c.problems());
        assert_eq!(c.languages(), vec!["en", "ru"]);
        let ru = c
            .report()
            .into_iter()
            .find(|r| r.meta.code == "ru")
            .expect("ru pack");
        assert_eq!(ru.missing, 0, "Russian is missing {} keys", ru.missing);
    }

    #[test]
    fn a_missing_locales_directory_is_not_a_boot_failure() {
        let catalog = Catalog::load("naw-no-such-locales-dir").expect("must not fail");
        assert!(catalog.languages().is_empty());
        assert_eq!(catalog.render("en", "a.key", &[]), "a.key");
    }
}
