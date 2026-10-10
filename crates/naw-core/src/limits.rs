//! Every limit the engine applies to content and people, in one place.
//!
//! Each limit has a default, a range it is clamped into, and a scope. The
//! value comes from, each overriding the one before:
//!
//! 1. the default below,
//! 2. `[limits]` in the configuration file,
//! 3. `NAW_LIMIT_<NAME>` in the environment (or in an env file the command
//!    line names),
//! 4. `--limit name=value` on the command line,
//! 5. for one wiki, its own settings, when the scope allows.
//!
//! Security bounds that keep the engine safe (image dimensions, redirect
//! hops, diff work, session and OAuth lifetimes) are not here on purpose:
//! they stay constants beside the code they protect.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who may change a limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The install only.
    Install,
    /// Each wiki, anywhere in the range.
    Wiki,
    /// Each wiki, but only down: the install pays for more.
    Lower,
}

/// One limit as the configuration, the admin panel and the docs describe it.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub name: &'static str,
    pub default: i64,
    pub min: i64,
    pub max: i64,
    pub scope: Scope,
    pub about: &'static str,
}

impl Spec {
    /// The environment variable that sets it.
    pub fn env_name(&self) -> String {
        format!("NAW_LIMIT_{}", self.name.to_ascii_uppercase())
    }
}

/// A limit in the configuration file: a number, or text such as `"20MiB"`.
fn number_or_text<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: TryFrom<i64>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(i64),
        Text(String),
    }
    let n = match Raw::deserialize(deserializer)? {
        Raw::Number(n) => n,
        Raw::Text(text) => parse_number(&text).ok_or_else(|| {
            serde::de::Error::custom(format!("{text:?} is not a number such as 5000 or 20MiB"))
        })?,
    };
    // Out of range for the type means out of range for the limit: the
    // smallest value, clamped up later.
    Ok(T::try_from(n)
        .or_else(|_| T::try_from(0))
        .unwrap_or_else(|_| unreachable!("0 fits every limit type")))
}

const KIB: i64 = 1024;
const MIB: i64 = 1024 * 1024;
const GIB: i64 = 1024 * 1024 * 1024;

macro_rules! limits {
    ($( $(#[doc = $about:literal])+ $name:ident: $ty:ty = $default:expr, $min:expr, $max:expr, $scope:ident; )+) => {
        /// The engine's limits; see the module documentation.
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct Limits {
            $( $(#[doc = $about])+ #[serde(deserialize_with = "number_or_text")] pub $name: $ty, )+
        }

        impl Default for Limits {
            fn default() -> Self {
                Self { $( $name: ($default) as $ty, )+ }
            }
        }

        /// Every limit, in the order the docs and the admin panel show them.
        pub const SPECS: &[Spec] = &[
            $( Spec {
                name: stringify!($name),
                default: $default,
                min: $min,
                max: $max,
                scope: Scope::$scope,
                about: concat!($($about),+),
            }, )+
        ];

        impl Limits {
            /// One limit by name.
            pub fn get(&self, name: &str) -> Option<i64> {
                match name {
                    $( stringify!($name) => Some(self.$name as i64), )+
                    _ => None,
                }
            }

            /// Sets one limit by name, clamped into its range. `false` for a
            /// name that is not a limit.
            pub fn set(&mut self, name: &str, value: i64) -> bool {
                match name {
                    $( stringify!($name) => {
                        self.$name = value.clamp($min, $max) as $ty;
                        true
                    } )+
                    _ => false,
                }
            }
        }
    };
}

limits! {
    /// Largest article text, in bytes.
    page_bytes: usize = 5 * MIB, 64 * KIB, 32 * MIB, Lower;
    /// Longest page title, in characters.
    page_title_chars: usize = 200, 20, 500, Wiki;
    /// Longest edit summary, in characters.
    edit_summary_chars: usize = 200, 20, 1000, Wiki;
    /// Longest page address, in characters.
    page_address_chars: usize = 100, 20, 200, Install;

    /// Most levels a category name may have, as in Streams/ARG/2024.
    category_levels: usize = 6, 1, 16, Wiki;
    /// Longest category address, in characters.
    category_key_chars: usize = 100, 20, 200, Wiki;
    /// Most pages one category page lists.
    category_pages_shown: i64 = 5000, 100, 50000, Lower;

    /// Templates inside templates, at most this deep.
    template_depth: usize = 16, 2, 64, Lower;
    /// Template and function calls in one page.
    template_calls: usize = 2000, 50, 20000, Lower;
    /// Longest text a page may expand to, in bytes.
    template_output_bytes: usize = 12 * MIB, MIB, 64 * MIB, Lower;
    /// Different templates one page may use.
    templates_per_page: usize = 200, 10, 2000, Lower;
    /// Pages a template's page lists as using it.
    template_uses_shown: i64 = 50, 10, 1000, Wiki;

    /// Edits Recent changes shows.
    recent_changes_shown: i64 = 100, 10, 1000, Wiki;
    /// Titles on one screen of All pages.
    all_pages_shown: i64 = 300, 50, 2000, Wiki;
    /// Rows on the other special pages that list pages.
    system_list_shown: i64 = 200, 20, 2000, Wiki;
    /// Files on one screen of the file lists.
    files_shown: i64 = 120, 12, 1000, Wiki;
    /// Revisions on one screen of a page history.
    history_per_page: i64 = 50, 10, 500, Wiki;
    /// Results on one screen of search.
    search_results: i64 = 25, 5, 200, Wiki;
    /// Pages a file's page lists as using it.
    file_uses_shown: i64 = 100, 10, 1000, Wiki;
    /// An edit this many bytes or more either way is shown in bold.
    big_edit_bytes: i64 = 500, 50, 100000, Wiki;

    /// Drafts one person may keep on one wiki.
    drafts_per_person: i64 = 50, 1, 1000, Wiki;

    /// Longest report message, in characters.
    report_message_chars: usize = 4000, 100, 20000, Wiki;
    /// Reports one person may send in a burst.
    report_burst: i64 = 5, 1, 100, Wiki;
    /// How long a burst lasts, in minutes.
    report_burst_minutes: i64 = 10, 1, 1440, Wiki;
    /// Open reports one person may have on one wiki.
    reports_open_per_person: i64 = 20, 1, 500, Wiki;

    /// Days an account counts as new; 0 turns the rules for new accounts off.
    newcomer_days: i64 = 4, 0, 365, Wiki;
    /// Accepted edits after which an account is no longer new, if it is old enough too.
    newcomer_edits: i64 = 10, 0, 10000, Wiki;
    /// Minutes a new account waits before its first edit.
    newcomer_first_edit_minutes: i64 = 5, 0, 1440, Wiki;
    /// Outside links one edit by a new account may add.
    newcomer_links_per_edit: i64 = 3, 0, 1000, Wiki;
    /// Pages a new account may start in a day.
    newcomer_new_pages_per_day: i64 = 3, 1, 1000, Wiki;

    /// Uploads one person may make in a day, versions included.
    uploads_per_day: i64 = 300, 1, 100000, Lower;
    /// Bytes one person may upload in a day.
    upload_bytes_per_day: i64 = 2 * GIB, 16 * MIB, 1024 * GIB, Lower;
    /// Longest note on a file version or a hiding reason, in characters.
    file_note_chars: usize = 300, 20, 2000, Wiki;

    /// Pages one address may open in a minute.
    rate_read_per_minute: i64 = 600, 60, 100000, Install;
    /// Searches, histories, diffs and old revisions one address may open in a minute.
    rate_heavy_per_minute: i64 = 60, 10, 10000, Install;
    /// Previews, draft saves and suggestions one address may ask for in a minute.
    rate_typing_per_minute: i64 = 240, 20, 10000, Install;
    /// Other forms one address may send in a minute.
    rate_write_per_minute: i64 = 30, 5, 10000, Install;
}

static INSTALL: std::sync::OnceLock<Limits> = std::sync::OnceLock::new();

/// Records the install's limits once, at start, for code that runs with no
/// request to read them from (address checks, body size layers).
pub fn set_install(limits: Limits) {
    let _ = INSTALL.set(limits);
}

/// The install's limits, or the defaults before [`set_install`].
pub fn install() -> &'static Limits {
    static DEFAULT: std::sync::OnceLock<Limits> = std::sync::OnceLock::new();
    INSTALL
        .get()
        .unwrap_or_else(|| DEFAULT.get_or_init(Limits::default))
}

/// Why a limit could not be set.
#[derive(Debug, PartialEq, Eq)]
pub enum LimitError {
    Unknown(String),
    NotANumber { name: String, value: String },
}

impl std::fmt::Display for LimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "{name} is not a limit (see `naw limits`)"),
            Self::NotANumber { name, value } => {
                write!(f, "limit {name} must be a whole number, got {value:?}")
            }
        }
    }
}

impl std::error::Error for LimitError {}

/// A number as a person writes it: `5000`, `5_000`, `20MiB`, `2GiB`, `64k`.
pub fn parse_number(raw: &str) -> Option<i64> {
    let clean: String = raw.trim().chars().filter(|c| *c != '_').collect();
    let lower = clean.to_ascii_lowercase();
    let (digits, unit) = match lower.find(|c: char| !c.is_ascii_digit() && c != '-') {
        Some(at) => lower.split_at(at),
        None => (lower.as_str(), ""),
    };
    let n: i64 = digits.parse().ok()?;
    let factor = match unit.trim() {
        "" => 1,
        "k" | "kb" | "kib" => KIB,
        "m" | "mb" | "mib" => MIB,
        "g" | "gb" | "gib" => GIB,
        _ => return None,
    };
    n.checked_mul(factor)
}

impl Limits {
    /// Every value clamped into its range, as after [`Limits::set`].
    pub fn clamped(mut self) -> Self {
        for spec in SPECS {
            if let Some(value) = self.get(spec.name) {
                self.set(spec.name, value);
            }
        }
        self
    }

    /// Sets one limit from text, as the environment and the command line give it.
    pub fn set_text(&mut self, name: &str, raw: &str) -> Result<(), LimitError> {
        let name = name.trim().to_ascii_lowercase().replace('-', "_");
        if self.get(&name).is_none() {
            return Err(LimitError::Unknown(name));
        }
        let value = parse_number(raw).ok_or_else(|| LimitError::NotANumber {
            name: name.clone(),
            value: raw.to_string(),
        })?;
        self.set(&name, value);
        Ok(())
    }

    /// `NAW_LIMIT_<NAME>` from `lookup`, usually the process environment.
    pub fn apply_env(&mut self, lookup: impl Fn(&str) -> Option<String>) -> Result<(), LimitError> {
        for spec in SPECS {
            if let Some(raw) = lookup(&spec.env_name()) {
                self.set_text(spec.name, &raw)?;
            }
        }
        Ok(())
    }

    /// `name=value` pairs, as `--limit` gives them.
    pub fn apply_pairs<'a>(
        &mut self,
        pairs: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), LimitError> {
        for pair in pairs {
            let (name, value) = pair.split_once('=').ok_or_else(|| LimitError::NotANumber {
                name: pair.to_string(),
                value: String::new(),
            })?;
            self.set_text(name, value)?;
        }
        Ok(())
    }

    /// The limits for one wiki: these, with what its `settings.limits` may
    /// change. A value out of range is clamped; one a wiki may only lower
    /// never rises above the install's; anything malformed is ignored.
    pub fn for_wiki(&self, settings: &Value) -> Self {
        let mut out = self.clone();
        let Some(block) = settings.get("limits").and_then(Value::as_object) else {
            return out;
        };
        for spec in SPECS {
            let Some(value) = block.get(spec.name).and_then(Value::as_i64) else {
                continue;
            };
            let install = self.get(spec.name).unwrap_or(spec.default);
            match spec.scope {
                Scope::Install => {}
                Scope::Wiki => {
                    out.set(spec.name, value);
                }
                Scope::Lower => {
                    out.set(spec.name, value.min(install));
                }
            }
        }
        out
    }

    /// The highest value a wiki may choose for `spec`.
    pub fn wiki_ceiling(&self, spec: &Spec) -> i64 {
        match spec.scope {
            Scope::Lower => self.get(spec.name).unwrap_or(spec.default),
            _ => spec.max,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_default_is_in_its_range() {
        for spec in SPECS {
            assert!(
                (spec.min..=spec.max).contains(&spec.default),
                "{} default {} outside {}..={}",
                spec.name,
                spec.default,
                spec.min,
                spec.max
            );
            assert_eq!(Limits::default().get(spec.name), Some(spec.default));
        }
        assert_eq!(Limits::default().clamped(), Limits::default());
    }

    #[test]
    fn layers_override_in_order_and_values_are_clamped() {
        let mut limits: Limits =
            toml::from_str("category_levels = 3\nfiles_shown = 5").expect("toml");
        let limits_clamped = limits.clone().clamped();
        assert_eq!(limits_clamped.files_shown, 12, "clamped to the minimum");
        assert_eq!(limits.category_levels, 3);
        limits
            .apply_env(|name| (name == "NAW_LIMIT_CATEGORY_LEVELS").then(|| "8".to_string()))
            .expect("env");
        assert_eq!(limits.category_levels, 8);
        limits
            .apply_pairs(["category-levels=10", "page_bytes=8MiB"])
            .expect("pairs");
        assert_eq!(limits.category_levels, 10);
        assert_eq!(limits.page_bytes, 8 * 1024 * 1024);
        assert_eq!(
            limits.set_text("nope", "1"),
            Err(LimitError::Unknown("nope".into()))
        );
        assert!(limits.set_text("category_levels", "many").is_err());
        assert!(toml::from_str::<Limits>("no_such_limit = 1").is_err());
        let text: Limits = toml::from_str("page_bytes = \"8MiB\"").expect("text value");
        assert_eq!(text.page_bytes, 8 * 1024 * 1024);
        assert!(toml::from_str::<Limits>("page_bytes = \"lots\"").is_err());
    }

    #[test]
    fn a_wiki_may_lower_costly_limits_but_never_raise_them() {
        let install = Limits::default();
        let wiki = install.for_wiki(&json!({ "limits": {
            "category_levels": 12,
            "page_bytes": 64 * 1024 * 1024,
            "template_depth": 4,
            "page_address_chars": 30,
            "files_shown": "lots"
        }}));
        assert_eq!(wiki.category_levels, 12, "a wiki choice");
        assert_eq!(
            wiki.page_bytes, install.page_bytes,
            "never above the install"
        );
        assert_eq!(wiki.template_depth, 4, "lowering is fine");
        assert_eq!(wiki.page_address_chars, 100, "install only");
        assert_eq!(
            wiki.files_shown, install.files_shown,
            "malformed is ignored"
        );
        assert_eq!(install.for_wiki(&json!({})), install);
    }

    #[test]
    fn every_limit_is_documented_and_named_in_every_language() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let docs = std::fs::read_to_string(root.join("docs/configuration.md")).expect("docs");
        for spec in SPECS {
            assert!(
                docs.contains(&format!("| `{}` |", spec.name)),
                "docs/configuration.md has no row for {}",
                spec.name
            );
        }
        for lang in ["en", "ru"] {
            let raw = std::fs::read_to_string(root.join(format!("locales/{lang}/limits.toml")))
                .expect("locale");
            let table: toml::Table = toml::from_str(&raw).expect("toml");
            for spec in SPECS {
                assert!(
                    table.contains_key(spec.name),
                    "locales/{lang}/limits.toml has no {}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn numbers_read_as_people_write_them() {
        assert_eq!(parse_number("5000"), Some(5000));
        assert_eq!(parse_number("5_000"), Some(5000));
        assert_eq!(parse_number("20MiB"), Some(20 * MIB));
        assert_eq!(parse_number(" 2 GiB "), Some(2 * GIB));
        assert_eq!(parse_number("64k"), Some(64 * KIB));
        assert_eq!(parse_number("ten"), None);
        assert_eq!(parse_number("5 parsecs"), None);
    }
}
