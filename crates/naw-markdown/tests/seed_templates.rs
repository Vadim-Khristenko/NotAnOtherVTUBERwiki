//! The templates the seeds ship, expanded and rendered as a page would be:
//! the notices keep their markup through the sanitizer, the Markdown inside
//! them renders, and they put pages in the categories their docs promise.

use std::collections::HashMap;

use naw_markdown::categories;
use naw_markdown::transclude::{self, Notes};

const SEED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../seeds/filian/template");

fn templates() -> HashMap<String, String> {
    let mut out = HashMap::new();
    for entry in std::fs::read_dir(SEED).expect("seed dir") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        // the main versions only: a translation brings labels, not code
        if let Some(slug) = name.strip_suffix(".md").filter(|s| !s.contains('.')) {
            out.insert(
                slug.to_string(),
                std::fs::read_to_string(&path).expect("read"),
            );
        }
    }
    out
}

fn page(source: &str) -> (String, Vec<String>) {
    let expanded = transclude::expand(source, &templates(), &Notes::default());
    assert!(
        expanded.missing.is_empty(),
        "missing {:?}",
        expanded.missing
    );
    let keys = categories::of(&expanded.text)
        .into_iter()
        .map(|m| m.key)
        .collect();
    (naw_markdown::render_html(&expanded.text), keys)
}

#[test]
fn notices_render_with_their_class_and_markdown_inside() {
    for (call, class) in [
        ("{{Update|reason=Old schedule.}}", "wiki-notice-update"),
        ("{{Stub}}", "wiki-notice-stub"),
        ("{{Sources needed}}", "wiki-notice-sources"),
        ("{{Cleanup}}", "wiki-notice-cleanup"),
        ("{{In progress|Akane}}", "wiki-notice-progress"),
        ("{{Disputed}}", "wiki-notice-disputed"),
        ("{{Spoiler|what=the finale}}", "wiki-notice-spoiler"),
        ("{{Speculation}}", "wiki-notice-speculation"),
        ("{{Fan wiki}}", "wiki-notice-fan"),
    ] {
        let (html, _) = page(&format!("{call}\n\nText.\n"));
        assert!(
            html.contains(&format!("class=\"wiki-notice {class}\"")),
            "{call}: {html}"
        );
        assert!(
            html.contains("<strong>"),
            "{call}: bold title renders: {html}"
        );
        assert!(
            !html.contains("{{"),
            "{call}: nothing left unexpanded: {html}"
        );
        assert!(
            !html.contains("[[Category"),
            "{call}: no category text shows: {html}"
        );
    }
    let (html, _) = page("{{Update|reason=Old schedule.}}\n");
    assert!(html.contains("Old schedule."), "{html}");
    // `{{#if:x| text}}` trims its branch, so the space must sit outside it.
    let (html, _) = page("{{Update|reason=Old schedule.|since=2026-09}}\n");
    assert!(
        html.contains("Old schedule. <em>Since 2026-09.</em>"),
        "{html}"
    );
    let (html, _) =
        page("{{Infobox event\n```yaml\nname: E\ndate: 2026-01-01\nend: 2026-01-03\n```\n}}\n");
    assert!(html.contains("2026-01-01 … 2026-01-03"), "{html}");
}

#[test]
fn maintenance_notices_file_the_page_and_nocat_does_not() {
    let (_, keys) = page("{{Update}}\n\n{{Stub}}\n\nA claim.{{Citation needed}}\n");
    assert_eq!(
        keys,
        [
            "maintenance:pages-to-update",
            "maintenance:stubs",
            "maintenance:pages-needing-sources"
        ]
    );
    let (_, none) = page("{{Update|nocat=yes}}\n\n{{Stub|nocat=yes}}\n");
    assert!(none.is_empty(), "{none:?}");
}

#[test]
fn infoboxes_hatnotes_quotes_and_navboxes_render() {
    let (html, keys) = page(
        "{{Infobox stream\n```yaml\ntitle: A stream\ndate: 2026-01-01\nvod: https://example.com/v\n```\n}}\n\nText.\n",
    );
    assert!(html.contains("infobox"), "{html}");
    assert!(
        html.contains("A stream") && html.contains("href=\"https://example.com/v\""),
        "{html}"
    );
    assert_eq!(keys, ["streams"]);

    let (html, _) = page("{{Main|lore|Lore}}\n");
    assert!(
        html.contains("class=\"wiki-hatnote\"") && html.contains("href=\"lore\""),
        "{html}"
    );

    let (html, _) = page("{{Quote|Exact words.|by=Someone}}\n");
    assert!(
        html.contains("Exact words.") && html.contains("<cite>Someone</cite>"),
        "{html}"
    );

    let (html, _) = page(
        "{{Navbox\n```yaml\ntitle: Series\nitems:\n  - \"[One](one)\"\n  - \"[Two](two)\"\n```\n}}\n",
    );
    assert!(
        html.contains("class=\"wiki-navbox\"") && html.contains("href=\"two\""),
        "{html}"
    );

    let (html, _) = page("A claim.{{Citation needed}}\n");
    assert!(
        html.contains("class=\"wiki-cn\"") && html.contains("[citation needed]"),
        "{html}"
    );
}
