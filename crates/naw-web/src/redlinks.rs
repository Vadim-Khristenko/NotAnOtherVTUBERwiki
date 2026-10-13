//! Links to pages that do not exist yet, marked as MediaWiki marks them in
//! red, so a reader sees before clicking that nothing is there and a writer
//! sees in the preview which links lead nowhere.
//!
//! Whether a page exists changes after a body is rendered, so this runs
//! when a page is served, after the render cache, as emotes and thumbnails
//! do. One query per page asks for every page it links to; a page in any
//! language, or a redirect, counts as there.

use std::collections::HashSet;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::resolve::Ctx;

/// The class a link to a missing page gets.
const CLASS: &str = "wikilink-new";

/// Links to at most this many different pages are checked; a longer list
/// is left as it is rather than made into one huge query.
const CHECKED_MAX: usize = 500;

/// Marks the links in `html` whose page does not exist.
pub(crate) async fn expand(state: &AppState, ctx: &Ctx, html: String) -> Result<String, AppError> {
    let mut wanted: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for tag in anchors(&html) {
        if let Some(target) = href(tag).and_then(page_target)
            && seen.insert(target.clone())
        {
            wanted.push(target);
        }
    }
    if wanted.is_empty() || wanted.len() > CHECKED_MAX {
        return Ok(html);
    }
    let (namespaces, slugs): (Vec<String>, Vec<String>) = wanted.iter().cloned().unzip();
    let found = sqlx::query!(
        r#"SELECT w.namespace AS "namespace!", w.slug AS "slug!"
           FROM unnest($2::text[], $3::text[]) AS w(namespace, slug)
           WHERE EXISTS (SELECT 1 FROM pages p
                         WHERE p.wiki_id = $1 AND p.namespace::text = w.namespace AND p.slug = w.slug
                           AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL)
              OR EXISTS (SELECT 1 FROM page_redirects r
                         WHERE r.wiki_id = $1 AND r.namespace::text = w.namespace AND r.from_slug = w.slug)"#,
        ctx.wiki.id,
        &namespaces,
        &slugs
    )
    .fetch_all(&state.db)
    .await?;
    let existing: HashSet<(String, String)> =
        found.into_iter().map(|r| (r.namespace, r.slug)).collect();
    if existing.len() == wanted.len() {
        return Ok(html);
    }
    let title = naw_core::html::escape(&ctx.t("page.missing_title"));
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html.as_str();
    while let Some(at) = rest.find("<a ") {
        out.push_str(&rest[..at]);
        let end = rest[at..].find('>').map_or(rest.len(), |e| at + e);
        let tag = &rest[at..end];
        let missing = href(tag)
            .and_then(page_target)
            .is_some_and(|target| !existing.contains(&target));
        if missing && !tag.contains(" class=") {
            out.push_str(&format!("<a class=\"{CLASS}\" title=\"{title}\" "));
            out.push_str(&tag["<a ".len()..]);
        } else {
            out.push_str(tag);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Every `<a ...` tag up to its `>`.
fn anchors(html: &str) -> impl Iterator<Item = &str> {
    html.match_indices("<a ").map(move |(at, _)| {
        let end = html[at..].find('>').map_or(html.len(), |e| at + e);
        &html[at..end]
    })
}

fn href(tag: &str) -> Option<&str> {
    let start = tag.find(" href=\"")? + " href=\"".len();
    let end = tag[start..].find('"')?;
    Some(&tag[start..start + end])
}

/// The page an internal link leads to, as namespace and slug: `bao`,
/// `/bao#lore` and `template:infobox` do; addresses elsewhere, anchors,
/// files and categories do not.
fn page_target(href: &str) -> Option<(String, String)> {
    let path = href.split('#').next()?;
    if path.is_empty() || path.contains(['?', '&']) || path.starts_with("//") {
        return None;
    }
    let path = path.strip_prefix("./").unwrap_or(path);
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.is_empty() || path.contains('/') || path.contains("://") || path.starts_with("mailto:")
    {
        return None;
    }
    if !crate::pages::slug_is_valid(path) {
        return None;
    }
    match crate::pages::split_path(path) {
        (namespace @ ("main" | "template" | "page_template" | "module"), slug) => {
            Some((namespace.to_string(), slug.to_string()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_links_name_their_page_and_others_do_not() {
        let t = |h: &str| page_target(h).map(|(n, s)| format!("{n}/{s}"));
        assert_eq!(t("bao-the-whale").as_deref(), Some("main/bao-the-whale"));
        assert_eq!(t("/lore#arg").as_deref(), Some("main/lore"));
        assert_eq!(
            t("template:infobox-vtuber").as_deref(),
            Some("template/infobox-vtuber")
        );
        assert_eq!(t("#trivia"), None);
        assert_eq!(t("https://example.com/x"), None);
        assert_eq!(t("/ru/lore"), None);
        assert_eq!(t("./category:vtubers"), None);
        assert_eq!(t("image:bao.png"), None);
        assert_eq!(t("/search?q=x"), None);
        assert_eq!(t("Bao the Whale"), None);
    }

    #[test]
    fn no_address_outside_the_wiki_is_ever_taken_for_a_page() {
        for href in [
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            "data:text/html,x",
            "//evil.example/x",
            "\\\\evil.example",
            "/../admin",
            "..%2fadmin",
            "bao%00",
            "bao\u{0}",
            "bao?x=1",
            "bao&x",
            "mailto:a@b",
            "ftp://x",
            "",
            "#",
            "/",
            "./",
            "bao/edit",
            "Bao",
            "bao-the-whale-\u{202e}",
        ] {
            if let Some((ns, slug)) = page_target(href) {
                assert!(
                    slug.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "{href:?} -> {ns}/{slug}"
                );
            }
        }
        // And any text at all goes through without a panic.
        let mut seed: u64 = 11;
        let pieces = [
            "/",
            ".",
            ":",
            "#",
            "?",
            "%",
            "a",
            "-",
            "ж",
            "template:",
            "module:",
            "\u{0}",
            "//",
            " ",
        ];
        for _ in 0..3000 {
            let mut s = String::new();
            for _ in 0..8 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                s.push_str(pieces[(seed >> 33) as usize % pieces.len()]);
            }
            let _ = page_target(&s);
        }
    }

    #[test]
    fn anchors_and_hrefs_are_read_from_rendered_html() {
        let html = "<p>See <a href=\"bao\">Bao</a> and <a rel=\"x\" href=\"/lore\">lore</a>.</p>";
        let hrefs: Vec<_> = anchors(html).filter_map(href).collect();
        assert_eq!(hrefs, vec!["bao", "/lore"]);
    }
}
