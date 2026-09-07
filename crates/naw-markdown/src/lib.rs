//! NotAnotherWiki Engine Markdown render pipeline.
//!
//! Reader path contract: render on write, serve from cache. This function is
//! the pure core of that pipeline: Markdown in, sanitized HTML out.

/// Renders Markdown to sanitized HTML.
///
/// Tables, footnotes and task lists are enabled. Raw HTML is disabled at the
/// parser level (decision D12), and the result is sanitized with ammonia
/// regardless, so every byte of output passed the whitelist.
pub fn render_html(markdown: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);

    // Raw HTML is dropped here (decision D12). pulldown-cmark has no option
    // that refuses raw HTML, so the HTML events never reach the writer,
    // except a bare line break: `<br>` cannot execute anything.
    // ammonia is the second line of defence, not the first.
    let parser = Parser::new_ext(markdown, options).filter(|event| match event {
        Event::Html(html) | Event::InlineHtml(html) => is_allowed_raw_html(html),
        _ => true,
    });

    let mut dirty = String::with_capacity(markdown.len());
    pulldown_cmark::html::push_html(&mut dirty, parser);
    let anchored = add_heading_ids(&dirty);
    // `id` and `class` join the generic whitelist so heading anchors and
    // author styling survive. Neither executes anything; the worst a class
    // does is collide with site styles, which is the author's own choice.
    ammonia::Builder::default()
        .add_generic_attributes(["id", "class"])
        .clean(&anchored)
        .to_string()
}

/// The only raw HTML tag the parser lets through. Everything else stays
/// dropped per decision D12.
fn is_allowed_raw_html(html: &str) -> bool {
    matches!(
        html.trim().to_ascii_lowercase().as_str(),
        "<br>" | "<br/>" | "<br />"
    )
}
/// Gives every `#`-heading a stable `id` so pages support `#fragment`
/// links and a future table of contents. Duplicate titles get `-2`, `-3`.
fn add_heading_ids(html: &str) -> String {
    use std::collections::HashMap;
    use std::fmt::Write;

    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut seen: HashMap<String, usize> = HashMap::new();
    while let Some(open) = rest.find("<h") {
        let level = rest[open + 2..].chars().next();
        let Some(n) = level
            .and_then(|c| c.to_digit(10))
            .filter(|n| (1..=6).contains(n))
        else {
            out.push_str(&rest[..open + 2]);
            rest = &rest[open + 2..];
            continue;
        };
        let tag_start = open + 3;
        let Some(tag_end) = rest[tag_start..].find('>') else {
            break;
        };
        let content_start = tag_start + tag_end + 1;
        let close = format!("</h{n}>");
        let Some(content_end) = rest[content_start..].find(&close) else {
            break;
        };
        let inner = &rest[content_start..content_start + content_end];
        let (inner, attr_id, attr_class) = split_heading_attrs(inner);
        let text: String = strip_inline_tags(inner);
        let base = attr_id
            .filter(|id| !id.is_empty())
            .map(|id| id.to_string())
            .unwrap_or_else(|| slugify(&text));
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        let id = if *count == 1 {
            base
        } else {
            format!("{}-{}", base, count)
        };
        if id.is_empty() {
            let _ = write!(out, "{}<h{n}>{inner}{close}", &rest[..open],);
        } else if let Some(class) = attr_class {
            let _ = write!(
                out,
                "{}<h{n} id=\"{id}\" class=\"{class}\">{inner}{close}",
                &rest[..open],
            );
        } else {
            let _ = write!(out, "{}<h{n} id=\"{id}\">{inner}{close}", &rest[..open],);
        }
        rest = &rest[content_start + content_end + close.len()..];
    }
    out.push_str(rest);
    out
}

/// Splits a trailing `{key="value" ...}` block off heading HTML. Only `id`
/// and `class` survive, anything else is dropped. A malformed block stays
/// literal text so authors always see what they wrote.
fn split_heading_attrs(inner: &str) -> (&str, Option<&str>, Option<&str>) {
    let trimmed = inner.trim_end();
    if !trimmed.ends_with('}') {
        return (inner, None, None);
    }
    let Some(block_start) = trimmed.rfind('{') else {
        return (inner, None, None);
    };
    let mut id = None;
    let mut class = None;
    let mut rest = trimmed[block_start + 1..trimmed.len() - 1].trim();
    let mut ok = true;
    while !rest.is_empty() && ok {
        let first_ok = rest
            .chars()
            .next()
            .map(|c| c.is_ascii_lowercase())
            .unwrap_or(false);
        let key_len: usize = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || *c == '-' || c.is_ascii_digit())
            .map(|c| c.len_utf8())
            .sum();
        let value = rest[key_len..]
            .strip_prefix("=\"")
            .and_then(|v| v.find('"').map(|end| &v[..end]));
        match (first_ok && key_len > 0, value) {
            (true, Some(val)) => {
                let key = &rest[..key_len];
                if key == "id" && id.is_none() && !val.is_empty() {
                    id = Some(val);
                } else if key == "class" && class.is_none() {
                    class = Some(val);
                }
                rest = rest[key_len + 2 + val.len() + 1..].trim_start();
            }
            _ => ok = false,
        }
    }
    if !ok {
        return (inner, None, None);
    }
    (trimmed[..block_start].trim_end(), id, class)
}

fn strip_inline_tags(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => text.push(c),
            _ => {}
        }
    }
    text
}

fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = true;
    for c in text.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// Version of the render pipeline. Part of the `render_cache` key: bump it
/// whenever `render_page` output changes for identical input. Skin and
/// chrome changes count: a new footer is a new rendering.
pub const RENDERER_VERSION: i32 = 3;

/// A fully rendered page plus the key it is cached under.
pub struct RenderedPage {
    pub content_hash: Vec<u8>,
    pub html: String,
}

/// Body-only content hash. Matches the `revisions.content_hash` semantics.
pub fn content_hash(body_md: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(body_md.as_bytes()).to_vec()
}

/// Cache key hash. Everything that renders into the page is part of the
/// key: correctness beats cross-page deduplication.
pub fn page_hash(title: &str, lang: &str, body_md: &str, summary: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(title.as_bytes());
    hasher.update([0u8]);
    hasher.update(lang.as_bytes());
    hasher.update([0u8]);
    hasher.update(body_md.as_bytes());
    hasher.update([0u8]);
    hasher.update(summary.as_bytes());
    hasher.finalize().to_vec()
}

/// Everything a full page render needs. One struct instead of a growing
/// parameter list, so template chrome never reshapes the call sites again.
pub struct PageInput<'a> {
    pub title: &'a str,
    pub body_md: &'a str,
    pub wiki_name: &'a str,
    pub lang: &'a str,
    pub version: &'a str,
    pub served_from_cache: bool,
    pub summary: &'a str,
}

/// Renders a full page through the `page.html` template. `version` and the
/// cache flag land in the footer; on a hit the caller passes
/// `served_from_cache` instead of re-rendering. The displayed duration
/// covers the Markdown stage, the template adds microseconds on top.
pub fn render_page(
    env: &minijinja::Environment,
    input: &PageInput<'_>,
) -> Result<RenderedPage, naw_core::error::AppError> {
    let content_hash = page_hash(input.title, input.lang, input.body_md, input.summary);
    let started = std::time::Instant::now();
    let body_html = render_html(input.body_md);
    let render_ms = started.elapsed().as_millis() as u64;
    let footer_note = if input.served_from_cache {
        "served from cache".to_string()
    } else {
        format!("rendered in {render_ms} ms")
    };
    let template = env.get_template("page.html").map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            title => input.title,
            wiki_name => input.wiki_name,
            lang => input.lang,
            body => body_html,
            version => input.version,
            footer_note => footer_note,
            summary => input.summary,
        })
        .map_err(template_error)?;
    Ok(RenderedPage { content_hash, html })
}

fn template_error(err: minijinja::Error) -> naw_core::error::AppError {
    tracing::error!(error = %err, "template render error");
    naw_core::error::AppError::Internal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_and_paragraphs() {
        let html = render_html("# Title\n\nHello.");
        assert!(html.contains("<h1 id=\"title\">Title</h1>"));
        assert!(html.contains("<p>Hello.</p>"));
    }

    #[test]
    fn renders_tables() {
        let html = render_html("| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(html.contains("<table>"));
        assert!(html.contains("<td>1</td>"));
    }

    #[test]
    fn strips_raw_html() {
        let html = render_html("<script>alert(1)</script>");
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn drops_raw_html_that_sanitizing_would_keep() {
        // ammonia allows <div>, so only the parser level filter removes this.
        // If this test fails, D12 is not actually implemented.
        let html = render_html("<div>raw block</div>");
        assert!(!html.contains("<div"));
        assert!(!html.contains("raw block"));
    }

    #[test]
    fn keeps_emphasis() {
        let html = render_html("*Filian* is **fast**.");
        assert!(html.contains("<em>Filian</em>"));
        assert!(html.contains("<strong>fast</strong>"));
    }

    #[test]
    fn headings_keep_ids_after_sanitize() {
        let html = render_html("# Hello World\n\n## Hello World\n");
        assert!(html.contains("<h1 id=\"hello-world\">"));
        assert!(html.contains("<h2 id=\"hello-world-2\">"));
    }

    #[test]
    fn all_heading_levels_render() {
        let html =
            render_html("# One\n\n## Two\n\n### Three\n\n#### Four\n\n##### Five\n\n###### Six\n");
        for (n, slug) in ["1", "2", "3", "4", "5", "6"]
            .iter()
            .zip(["one", "two", "three", "four", "five", "six"])
        {
            assert!(html.contains(&format!("<h{n} id=\"{slug}\">")), "level {n}");
        }
    }

    #[test]
    fn footnotes_render() {
        let html = render_html("Text[^1].\n\n[^1]: The note.\n");
        assert!(html.contains("The note."));
        assert!(html.contains("href=\"#1\""));
    }

    #[test]
    fn br_tag_survives() {
        let html = render_html("line one\n<br/>\nline two");
        assert!(html.contains("<br"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn heading_attrs_apply() {
        let html = render_html("### Ours {id=\"smth\" class=\"highlighted\"}\n");
        assert!(html.contains("<h3 id=\"smth\" class=\"highlighted\">Ours</h3>"));
    }

    #[test]
    fn heading_attrs_unknown_dropped() {
        let html = render_html("### Ours {id=\"x\" status=\"new\"}\n");
        assert!(html.contains("<h3 id=\"x\">Ours</h3>"));
        assert!(!html.contains("status"));
    }

    fn test_env() -> minijinja::Environment<'static> {
        naw_core::templates::load_templates(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skins/default"
        ))
        .expect("skins/default must exist with layout, page and 404 templates")
    }

    fn test_input() -> PageInput<'static> {
        PageInput {
            title: "Home",
            body_md: "# Hi",
            wiki_name: "FilianWIKI",
            lang: "en",
            version: "0.1.0",
            served_from_cache: false,
            summary: "",
        }
    }

    #[test]
    fn page_renders_title_and_body() {
        let html = render_page(&test_env(), &test_input())
            .expect("render")
            .html;
        assert!(html.contains("<title>Home"));
        assert!(html.contains("<h1 id=\"hi\">Hi</h1>"));
        assert!(html.contains("FilianWIKI"));
        assert!(html.contains("0.1.0"));
        assert!(html.contains("rendered in "));
    }

    #[test]
    fn cached_footer_has_no_timing() {
        let mut input = test_input();
        input.served_from_cache = true;
        let html = render_page(&test_env(), &input).expect("render").html;
        assert!(html.contains("served from cache"));
        assert!(!html.contains("rendered in"));
    }

    #[test]
    fn summary_renders_as_lede() {
        let mut input = test_input();
        input.summary = "Short version.";
        let html = render_page(&test_env(), &input).expect("render").html;
        assert!(html.contains("<p class=\"page-summary\">Short version.</p>"));
        let plain = render_page(&test_env(), &test_input())
            .expect("render")
            .html;
        assert!(!plain.contains("<p class=\"page-summary\">"));
    }

    #[test]
    fn page_hash_is_stable_and_sensitive() {
        let a = page_hash("T", "en", "# Hi", "");
        assert_eq!(a, page_hash("T", "en", "# Hi", ""));
        assert_ne!(a, page_hash("T", "en", "# Bye", ""));
        assert_ne!(a, page_hash("Other", "en", "# Hi", ""));
        assert_ne!(a, page_hash("T", "en", "# Hi", "lede"));
    }

    #[test]
    fn missing_template_is_an_error() {
        let env = minijinja::Environment::new();
        assert!(render_page(&env, &test_input()).is_err());
    }
}
