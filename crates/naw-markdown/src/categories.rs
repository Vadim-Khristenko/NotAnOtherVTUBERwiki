//! Categories, as on MediaWiki: `[[Category:VTubers]]` anywhere in a page
//! puts it in the category and shows nothing where it is written.
//! `[[Category:VTubers|Filian]]` sorts the page under "Filian" there, and
//! `[[:Category:VTubers]]` is an ordinary link to the category.
//!
//! A category is known by its key: the name lowercased, with spaces and `_`
//! as `-`, in any script, so `[[Категория:Витуберы]]` and
//! `[[category:витуберы]]` are one category at `/category:витуберы`.
//!
//! A name can have levels: `Streams/ARG`, `Streams:ARG` and `Streams/Sub:ARG`
//! are all the category ARG inside Streams, keyed `streams:arg`. Its parent
//! needs no page of its own: Streams lists ARG among its subcategories.

/// The prefixes a category link starts with, lowercase.
pub const PREFIXES: [&str; 2] = ["category:", "категория:"];

/// How deep and how long a category may be: from the wiki's limits
/// (`category_levels`, `category_key_chars`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub levels: usize,
    pub key_chars: usize,
}

impl Shape {
    /// The widest any wiki may choose, for telling whether an address could
    /// be a category at all.
    pub const LOOSEST: Self = Self {
        levels: 16,
        key_chars: 200,
    };

    pub fn of(limits: &naw_core::limits::Limits) -> Self {
        Self {
            levels: limits.category_levels,
            key_chars: limits.category_key_chars,
        }
    }
}

impl Default for Shape {
    fn default() -> Self {
        Self::of(&naw_core::limits::Limits::default())
    }
}

/// A level written only to say "inside", as in `Streams/Sub:ARG`.
const SUB_MARKERS: [&str; 3] = ["sub", "подкатегория", "под"];

/// One level of a category: its key and its name as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    pub key: String,
    pub name: String,
}

/// One level's key: lowercase letters and digits of any script, other
/// runs as one `-`.
fn level_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.trim().to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if (c.is_whitespace() || matches!(c, '-' | '_' | '.' | '\''))
            && !out.is_empty()
            && !out.ends_with('-')
        {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The levels of a category name, with or without its prefix: split at `/`
/// and `:`, empty levels and "sub" markers left out.
pub fn levels(name: &str) -> Vec<Level> {
    levels_in(name, Shape::default())
}

/// [`levels`], at most as many as `shape` allows; deeper ones are dropped.
pub fn levels_in(name: &str, shape: Shape) -> Vec<Level> {
    let bare = strip_prefix(name).unwrap_or(name);
    let parts: Vec<&str> = bare.split(['/', ':']).collect();
    let last = parts.len().saturating_sub(1);
    parts
        .iter()
        .enumerate()
        .filter_map(|(i, raw)| {
            let key = level_key(raw);
            let marker = i < last && SUB_MARKERS.contains(&key.as_str());
            (!key.is_empty() && !marker).then(|| Level {
                key,
                name: display_name(raw),
            })
        })
        .take(shape.levels.max(1))
        .collect()
}

/// The keys of every level above `key`, outermost first: `a:b:c` has `a`
/// and `a:b`.
pub fn ancestors(key: &str) -> Vec<String> {
    let parts: Vec<&str> = key.split(':').collect();
    (1..parts.len()).map(|n| parts[..n].join(":")).collect()
}

/// One category a page is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    /// The address key: `витуберы`.
    pub key: String,
    /// The name as written, tidied: `Витуберы`.
    pub name: String,
    /// What the page sorts under in the category, when the author chose.
    pub sort: Option<String>,
}

/// The name after a category prefix, when `dest` has one.
fn strip_prefix(dest: &str) -> Option<&str> {
    let trimmed = dest.trim_start();
    for prefix in PREFIXES {
        // Compared by characters: the Cyrillic prefix is not ASCII.
        let n = prefix.chars().count();
        let head: String = trimmed.chars().take(n).collect();
        if head.to_lowercase() == prefix {
            return Some(&trimmed[head.len()..]);
        }
    }
    None
}

/// The name tidied for showing: `_` as a space, runs of space as one.
pub fn display_name(name: &str) -> String {
    name.replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The key of a category name, with or without its prefix: its levels
/// joined by `:`. `None` when nothing usable is left or it is too long.
pub fn key(name: &str) -> Option<String> {
    key_in(name, Shape::default())
}

/// [`key`], within `shape`.
pub fn key_in(name: &str, shape: Shape) -> Option<String> {
    let key = levels_in(name, shape)
        .into_iter()
        .map(|l| l.key)
        .collect::<Vec<_>>()
        .join(":");
    (!key.is_empty() && key.chars().count() <= shape.key_chars).then_some(key)
}

/// Whether `dest` is a category link, `[[Category:X]]`, rather than a
/// link to the category, `[[:Category:X]]`.
pub(crate) fn is_membership(dest: &str) -> bool {
    strip_prefix(dest).is_some()
}

/// The key a `[[:Category:X]]` link points at.
pub(crate) fn link_target(dest: &str) -> Option<String> {
    let rest = dest.trim_start().strip_prefix(':')?;
    strip_prefix(rest).and_then(key)
}

/// The categories a page is in, in the order first written, once each.
pub fn of(markdown: &str) -> Vec<Membership> {
    of_in(markdown, Shape::default())
}

/// [`of`], within `shape`: a name deeper than it allows keeps its outer
/// levels, and its name is written the same way, levels joined by `/`.
pub fn of_in(markdown: &str, shape: Shape) -> Vec<Membership> {
    use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};
    if !markdown.contains("[[") {
        return Vec::new();
    }
    let mut found: Vec<Membership> = Vec::new();
    // The link being read: its key and name, and its text so far.
    let mut open: Option<(String, String, bool, String)> = None;
    for event in Parser::new_ext(markdown, crate::parser_options()) {
        match event {
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { has_pothole },
                dest_url,
                ..
            }) => {
                if let Some(name) = strip_prefix(&dest_url)
                    && let Some(key) = key_in(name, shape)
                {
                    let written: Vec<String> =
                        levels_in(name, shape).into_iter().map(|l| l.name).collect();
                    open = Some((key, written.join("/"), has_pothole, String::new()));
                }
            }
            Event::Text(text) | Event::Code(text) if open.is_some() => {
                if let Some((_, _, _, buf)) = open.as_mut() {
                    buf.push_str(&text);
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some((key, name, has_pothole, text)) = open.take()
                    && !found.iter().any(|m| m.key == key)
                {
                    let sort = has_pothole
                        .then(|| display_name(&text))
                        .filter(|s| !s.is_empty());
                    found.push(Membership { key, name, sort });
                }
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_the_name_in_any_script() {
        assert_eq!(key("VTubers").as_deref(), Some("vtubers"));
        assert_eq!(key("Category:Snack  Lore").as_deref(), Some("snack-lore"));
        assert_eq!(key("Категория:Витуберы").as_deref(), Some("витуберы"));
        assert_eq!(
            key("категория: Друзья Филиан").as_deref(),
            Some("друзья-филиан")
        );
        assert_eq!(key("ホロライブ").as_deref(), Some("ホロライブ"));
        assert_eq!(key("Filian's_friends").as_deref(), Some("filian-s-friends"));
        assert_eq!(key("  ***  "), None);
        assert_eq!(key("Category:"), None);
    }

    #[test]
    fn a_name_with_levels_is_one_category_however_it_is_written() {
        for written in [
            "Streams/ARG",
            "Streams:ARG",
            "Category:Streams/Sub:ARG",
            "streams / arg",
            "Streams//ARG/",
        ] {
            assert_eq!(key(written).as_deref(), Some("streams:arg"), "{written}");
        }
        assert_eq!(
            key("Стримы/Подкатегория:ARG").as_deref(),
            Some("стримы:arg")
        );
        // a level that is only called "Sub" is still a category when it is the last
        assert_eq!(key("Streams/Sub").as_deref(), Some("streams:sub"));
        let names: Vec<String> = levels("Streams/Sub:ARG lore")
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["Streams", "ARG lore"]);
        assert_eq!(ancestors("a:b:c"), ["a", "a:b"]);
        assert!(ancestors("a").is_empty());
    }

    #[test]
    fn a_wiki_with_fewer_levels_keeps_the_outer_ones() {
        let shape = Shape {
            levels: 2,
            key_chars: 100,
        };
        let found = of_in("[[Category:Streams/Sub:ARG/2024]]\n", shape);
        assert_eq!(found[0].key, "streams:arg");
        assert_eq!(found[0].name, "Streams/ARG");
        let short = Shape {
            levels: 6,
            key_chars: 5,
        };
        assert_eq!(key_in("Streams", short), None, "too long for this wiki");
        assert_eq!(key_in("ARG", short).as_deref(), Some("arg"));
    }

    #[test]
    fn categories_are_found_outside_code_once_each() {
        let md = "Text [[Category:VTubers]] more.\n\n[[Категория:Витуберы|Филиан]]\n\n\
                  `[[Category:InCode]]`\n\n```\n[[Category:InFence]]\n```\n\n\
                  [[category:vtubers]] [[:Category:JustALink]] [[home]]\n";
        let found = of(md);
        let keys: Vec<&str> = found.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["vtubers", "витуберы"]);
        assert_eq!(found[0].name, "VTubers");
        assert_eq!(found[0].sort, None);
        assert_eq!(found[1].name, "Витуберы");
        assert_eq!(found[1].sort.as_deref(), Some("Филиан"));
    }
}
