//! A dump comes from another site, so its wikitext is untrusted: the
//! converter must not panic on any of it, and what it makes still goes
//! through the renderer, which must not let script through.

use naw_wikitext::{Options, convert};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

const PIECES: &[&str] = &[
    "[[",
    "]]",
    "[",
    "]",
    "{{",
    "}}",
    "{{{",
    "}}}",
    "|",
    "||",
    "!!",
    "{|",
    "|}",
    "|-",
    "|+",
    "'''",
    "''",
    "'''''",
    "=",
    "==",
    "\n",
    "\n\n",
    " ",
    "*",
    "#",
    ":",
    ";",
    "----",
    "<ref>",
    "</ref>",
    "<ref name=a/>",
    "<references/>",
    "<nowiki>",
    "</nowiki>",
    "<pre>",
    "</pre>",
    "<tabber>",
    "</tabber>",
    "|-|",
    "<gallery>",
    "</gallery>",
    "<infobox>",
    "</infobox>",
    "<data source=\"x\">",
    "</data>",
    "<label>",
    "</label>",
    "<!--",
    "-->",
    "__TOC__",
    "#REDIRECT",
    "{{#if:",
    "{{#tag:",
    "{{#invoke:",
    "{{PAGENAME}}",
    "File:",
    "Category:",
    "http://x",
    "https://",
    "<script>",
    "javascript:",
    "onerror=",
    "<img",
    ">",
    "<",
    "\"",
    "&",
    "&lt;",
    "\u{0}",
    "\u{e000}",
    "\u{e001}",
    "\u{e002}",
    "é",
    "Ж",
    "🐳",
    "\u{200d}",
    "\t",
];

#[test]
fn random_wikitext_never_panics() {
    let mut rng = Lcg(0xdeadbeef);
    let options = Options {
        title: "Test".into(),
        ..Options::default()
    };
    for _ in 0..4000 {
        let len = 1 + rng.next() % 50;
        let mut input = String::new();
        for _ in 0..len {
            input.push_str(PIECES[(rng.next() as usize) % PIECES.len()]);
        }
        let out = convert(&input, &options);
        // The stash's private markers never leak into the Markdown.
        assert!(
            !out.markdown.contains(['\u{e000}', '\u{e002}']),
            "a marker leaked for {input:?}: {:?}",
            out.markdown
        );
        let html = naw_markdown::render_html(&out.markdown).to_lowercase();
        assert!(!html.contains("<script"), "script for {input:?}: {html}");
    }
}

#[test]
fn deep_nesting_does_not_overflow() {
    let options = Options::default();
    for open in ["[[", "{{", "{{{", "{|\n", "<ref>", "<tabber>", "'''"] {
        let input = open.repeat(5_000);
        let _ = convert(&input, &options);
    }
    let _ = convert(&"{{a|".repeat(3_000), &options);
    let _ = convert(&"[[a|[[b|".repeat(3_000), &options);
}
