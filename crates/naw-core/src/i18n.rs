//! Interface translation.
//!
//! Messages live in language packs, `locales/<lang>/`, outside the code and
//! outside the skins, so a translator needs neither Rust nor a template and a
//! skin author does not fork the wording by restyling a page. A pack is a
//! `metadata.toml` plus one small file per area of the interface; see
//! `Catalog::load`.
//!
//! Templates call `{{ t("search.heading") }}`. The language is **not** passed
//! in: the registered function reads `lang` out of the render state, which is
//! already in every template context. That matters, because the alternative is
//! threading a locale through every single binding and forgetting it in one
//! place.
//!
//! Plurals are a separate function on purpose. English needs two forms and
//! Russian needs three, and half the audience for this engine reads Russian, so
//! "5 revisions" cannot be built by gluing an "s" onto a number.
//!
//! Nothing here can fail a render. A missing catalogue, a missing language and a
//! missing key all degrade: language, then the fallback language, then the key
//! itself. A key showing through on the page is ugly and obvious, which is what
//! you want from a missing translation. A 500 is not.

use std::collections::BTreeMap;
use std::sync::Arc;

use minijinja::value::{Kwargs, Value};
use minijinja::{Environment, Error, State};

use crate::error::AppError;

/// The language used when a message is missing from the requested one.
pub const FALLBACK: &str = "en";

/// Plural categories, CLDR names. Only the ones the supported languages use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plural {
    One,
    Few,
    Many,
    Other,
}

impl Plural {
    /// The suffix appended to a message key, as in `revisions.one`.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }
}

/// Which plural form a count takes in a language.
///
/// The Slavic rule is the reason this function exists. Russian picks a form from
/// the last digit and the last two digits together, so 1, 21 and 101 behave
/// alike, 2 to 4 form a second group, and 11 to 14 are an exception inside the
/// first: "1 правка", "2 правки", "5 правок", "11 правок", "21 правка".
///
/// Everything unlisted gets the English rule, which is the safest wrong answer:
/// two forms, singular for exactly one.
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
        // Japanese, Korean, Chinese, Turkish and Indonesian have one form for
        // every count. Using the English rule would invent a distinction the
        // language does not make.
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
    /// What they did: `maintainer`, `translator`, `reviewer`. Free text, shown
    /// as written.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// `metadata.toml`: who wrote a pack, what version it is, and whether to offer
/// it at all.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Meta {
    pub code: String,
    /// The language's name in English, for an operator reading the admin panel.
    pub name: String,
    /// The language's name in itself, for the switcher. A Russian reader looks
    /// for "Русский", not for "Russian".
    pub native_name: String,
    pub version: String,
    /// The engine release the pack was last checked against. Informational.
    #[serde(default)]
    pub engine: Option<String>,
    /// A disabled pack is loaded, so the admin panel can show it and its
    /// completeness, but is never offered to readers.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// `ltr` or `rtl`, for the dir attribute.
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
    /// What a pack built without a metadata file describes itself as. Only
    /// `from_pairs` uses it; a directory with no metadata.toml is not a pack.
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

/// What the admin panel shows about one pack.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PackReport {
    pub meta: Meta,
    pub messages: usize,
    /// Keys the fallback language has and this pack does not. The fallback
    /// itself reports zero.
    pub missing: usize,
    /// 0 to 100. Rounded down, so a pack reads 100 only when nothing is missing.
    pub percent: u8,
}

/// Every message, by language then by key, plus each pack's metadata.
#[derive(Debug, Default)]
pub struct Catalog {
    languages: BTreeMap<String, BTreeMap<String, String>>,
    meta: BTreeMap<String, Meta>,
    /// Why a pack was skipped, kept so the admin panel can say so. A pack with a
    /// syntax error must not take the site down, and must not vanish silently
    /// either: the translator needs to know which file and which line.
    problems: Vec<String>,
}

impl Catalog {
    /// Loads every language pack under `dir`.
    ///
    /// A pack is a directory holding `metadata.toml`. Every other `*.toml` in it
    /// is one area of the interface, and its file name is the key prefix, so
    /// `ru/search.toml` supplies `search.heading`. That keeps a pack in small
    /// files a translator can take one at a time, rather than one file with
    /// every string in the engine.
    ///
    /// Nothing here stops the engine starting. A missing directory, a pack with
    /// a broken file, a metadata file whose `code` disagrees with its directory:
    /// each is logged, recorded in `problems`, and skipped. The interface then
    /// falls back to English, and to the message key, which is visible and
    /// obvious rather than a 500.
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
        // Sorted, so the order problems are reported in is stable run to run.
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
        // The directory name is what `?lang=`, the cookie and the Accept-Language
        // match all use. A pack claiming to be `ru` from a directory called
        // `russian` would load and then never be selectable.
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

    /// Builds a catalogue straight from pairs. For tests, and for an install
    /// that wants to ship messages compiled in.
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

    /// Languages offered to readers: loaded, and enabled in their metadata.
    /// Sorted, so a language switcher is stable.
    pub fn languages(&self) -> Vec<&str> {
        self.languages
            .keys()
            .filter(|code| self.meta.get(*code).is_none_or(|m| m.enabled))
            .map(String::as_str)
            .collect()
    }

    /// Whether a language may be chosen. A disabled pack answers false: its
    /// messages stay loaded for the admin panel, but a reader cannot select it
    /// by cookie, by URL or by browser preference.
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

    /// Every loaded pack, enabled or not, with how complete it is against the
    /// fallback language.
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

/// Whether a pack supplies a reference key.
///
/// Plural keys are the subtle case. English writes `history.revisions.one` and
/// `.other`; Russian writes `.one`, `.few` and `.many`, and never `.other`,
/// because Russian has no such form. Counting Russian as missing `.other`
/// would report a complete translation as incomplete, so a plural key counts
/// as covered when the pack has any form of it.
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
    /// One message, following the fallback chain.
    ///
    /// The chain is: the exact language, then its base language so that `ru-RU`
    /// finds `ru`, then the fallback language, then `None`. Skipping the base
    /// language step would leave a browser sending `en-GB` with an entirely
    /// untranslated page.
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

    /// A message plus its interpolations, or the key when there is no message.
    pub fn render(&self, lang: &str, key: &str, args: &[(String, String)]) -> String {
        let template = self.lookup(lang, key).unwrap_or(key);
        interpolate(template, args)
    }

    /// The plural form of `key` for `n`, trying `key.<form>` then `key.other`
    /// then `key`.
    ///
    /// The `other` step is what keeps a half-finished translation readable: a
    /// catalogue with only `.one` and `.other` still renders in Russian, in the
    /// wrong grammatical number rather than as a raw key.
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

/// Picks an interface language from an `Accept-Language` header.
///
/// Returns the first language the catalogue actually has, in the order the
/// browser asked for. A tag with a region counts as a match for its base
/// language, so `ru-RU` picks `ru`, which is what browsers send.
///
/// `None` means nothing matched and the caller should fall back to the wiki's
/// own language. Malformed input yields `None` rather than an error: this value
/// arrives from the internet on every request.
pub fn negotiate(header: &str, catalog: &Catalog) -> Option<String> {
    negotiate_with(header, &|code: &str| catalog.has(code))
}

/// As `negotiate`, against any notion of "available". A wiki that switched a
/// language off must not have it picked from a browser header either, so the
/// web layer passes its own per-wiki predicate rather than the whole catalogue.
pub fn negotiate_with(header: &str, available: &dyn Fn(&str) -> bool) -> Option<String> {
    let mut candidates: Vec<(f32, usize, String)> = Vec::new();
    for (position, part) in header.split(',').enumerate() {
        let mut pieces = part.split(';');
        let Some(tag) = pieces.next().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        // A wildcard means "anything", which is not a preference we can act on.
        if tag == "*" {
            continue;
        }
        // q defaults to 1.0. An unparseable q is treated as absent rather than
        // as zero: a browser sending junk still wants that language.
        let quality = pieces
            .find_map(|piece| piece.trim().strip_prefix("q="))
            .and_then(|value| value.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        // q=0 explicitly means "not this one".
        if quality <= 0.0 {
            continue;
        }
        // The header position breaks ties, so equal-quality tags keep the order
        // the browser listed them in.
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

/// Escapes a value for HTML text and attribute content.
///
/// Messages carry markup, so `t()` hands MiniJinja a string already marked as
/// safe. That makes escaping the *interpolated* values this module's job, and
/// not a detail a template author can forget: `search.nothing` embeds a visitor
/// supplied query inside `<strong>`, and without this an article title of
/// `<img onerror=...>` in a search box would execute.
fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Replaces `{name}` with the matching argument, escaped.
///
/// Deliberately not a template engine. An unknown placeholder is left exactly
/// as it is rather than blanked, so a mismatch between a message and its call
/// site is visible on the page instead of silently producing a sentence with a
/// hole in it.
///
/// The message text itself is trusted: it comes from a file in the repository,
/// written by whoever runs the wiki. The arguments are not, and are escaped.
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
                    Some((_, value)) => out.push_str(&escape_html(value)),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                // An unbalanced brace. Emit the rest verbatim and stop.
                out.push('{');
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Turns nested TOML tables into dotted keys, so a translator can write
/// `[search]` / `heading = "..."` and the template asks for `search.heading`.
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
        // Numbers and booleans in a message file are almost certainly a
        // mistake, but rendering them is friendlier than dropping them.
        other => {
            out.insert(prefix, other.to_string());
        }
    }
}

/// Pulls the render language out of the template context.
///
/// Every page context carries `lang`, so this needs no extra plumbing. A
/// template rendered without it falls back rather than failing.
fn lang_of(state: &State) -> String {
    state
        .lookup("lang")
        .as_ref()
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| FALLBACK.to_string())
}

/// Turns MiniJinja keyword arguments into name/value pairs.
///
/// `assert_all_used` is what catches a typo: passing `count=3` to a message
/// that wants `{n}` is an error the author sees, not a sentence with a visible
/// `{n}` shipped to readers.
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
/// `t("key", name=value, ...)` is one message.
/// `tn("key", n, name=value, ...)` is a counted message; `{n}` is always
/// available inside it without being passed.
///
/// Both return a value **already marked safe**, because messages legitimately
/// carry markup: `search.nothing` wraps the query in `<strong>`. The safety is
/// only sound because `interpolate` escapes every argument, so the trusted half
/// (the message, from a file in the repository) and the untrusted half (the
/// arguments, often a visitor's search query) are treated differently.
///
/// Returning a plain `String` instead would force `| safe` at every call site
/// in every skin, and the first template author to write
/// `{{ t("search.nothing", query=q) | safe }}` without knowing the arguments
/// were escaped would have shipped an XSS. Making it safe by construction here
/// means a skin cannot get it wrong.
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
            // A negative count has no plural rule anywhere. Clamp rather than
            // panic on the cast: a count in a template is a length or a total,
            // so below zero is a bug upstream, not a language question.
            let n = n.max(0) as u64;
            Ok(Value::from_safe_string(for_tn.render_plural(
                &lang_of(state),
                key,
                n,
                &pairs,
            )))
        },
    );

    // Exposed so a template can mark up a language picker, and so
    // `{% if "ru" in languages %}` works.
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
        // A browser sends en-GB and ru-RU constantly. Without this step the
        // whole interface would fall back to English for every Russian reader
        // whose browser is specific about its region.
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
        // Visible and obvious beats silent and empty: somebody reading the page
        // reports "search.nope" and it gets translated.
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
        // The attack this closes. `search.nothing` wraps the query in <strong>,
        // t() hands the result to MiniJinja already marked safe, and the query
        // is whatever a visitor typed into a URL.
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
        // The message's own markup survives; only the argument was escaped.
        assert!(rendered.contains("<strong>"), "{rendered}");
        assert!(!rendered.contains("<img"), "{rendered}");
    }

    #[test]
    fn every_dangerous_character_in_an_argument_is_escaped() {
        assert_eq!(escape_html("<>&\"'"), "&lt;&gt;&amp;&quot;&#39;");
        // Cyrillic and emoji are not markup and must pass through untouched:
        // escaping them would mangle half the wiki's content.
        assert_eq!(escape_html("Филиан 🍪"), "Филиан 🍪");
        assert_eq!(escape_html(""), "");
    }

    #[test]
    fn an_unknown_placeholder_stays_visible_rather_than_vanishing() {
        // A mismatch between a message and its call site has to be findable.
        // Blanking it produces "Hello " and nobody notices for a year.
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
        // The rule that makes this worth a function. 1/21/101 behave alike,
        // 2 to 4 form a group, and 11 to 14 break out of the first group.
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
        // Only `.one` and `.other`, asked for a Russian "few". Falling through
        // to `.other` gives the wrong grammatical number; falling through to
        // the raw key would give "history.revisions" on the page.
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
        // What a Russian browser actually sends.
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
        // German first but wanted less than Russian. German is not in the
        // catalogue anyway, so the point is that q is read at all.
        assert_eq!(negotiate("de;q=0.5,ru;q=0.9", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate("ru;q=0.2,en;q=0.8", &c).as_deref(), Some("en"));
        // Equal quality keeps the browser's order.
        assert_eq!(negotiate("ru,en", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate("en,ru", &c).as_deref(), Some("en"));
    }

    #[test]
    fn an_explicit_refusal_is_honoured() {
        let c = catalog();
        // q=0 means "not this one", so English wins even though it is second.
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
        // This value comes straight off the internet on every request.
        let c = catalog();
        assert_eq!(negotiate("ru;q=not-a-number", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate("ru;q=", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate("  RU-ru  ;  q=0.9  ", &c).as_deref(), Some("ru"));
        assert_eq!(negotiate(&"x".repeat(5000), &c), None);
    }

    /// Writes a throwaway locales tree and returns its root. Each call gets its
    /// own directory, so tests running in parallel do not share files.
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
        // The metadata file is not an area: it contributes no keys.
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
        // It would load and then never be selectable, because the directory name
        // is what the cookie and the URL parameter match.
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
        // One translator's typo must not take English down with it.
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
        // Still visible to the operator, with its completeness.
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
        // English writes .one and .other; Russian writes .one, .few and .many
        // and has no .other at all. Counting that as missing would report a
        // finished translation as unfinished.
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
        // Only nav.sign_out is genuinely missing.
        assert_eq!(ru.missing, 1);
        assert_eq!(ru.percent, 75);
        let en = report.iter().find(|r| r.meta.code == "en").expect("en");
        assert_eq!((en.missing, en.percent), (0, 100));
    }

    #[test]
    fn the_shipped_packs_load_cleanly_and_russian_is_complete() {
        // Against the real files in the repository. A typo in a shipped
        // translation fails here rather than on the first request.
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
        // A fresh checkout with no translations has to start. It shows keys,
        // which is a visible prompt to add a catalogue, not an outage.
        let catalog = Catalog::load("naw-no-such-locales-dir").expect("must not fail");
        assert!(catalog.languages().is_empty());
        assert_eq!(catalog.render("en", "a.key", &[]), "a.key");
    }
}
