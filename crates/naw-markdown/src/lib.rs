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
    // that refuses raw HTML, so the HTML events never reach the writer.
    // ammonia is the second line of defence, not the first.
    let parser = Parser::new_ext(markdown, options)
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)));

    let mut dirty = String::with_capacity(markdown.len());
    pulldown_cmark::html::push_html(&mut dirty, parser);
    ammonia::Builder::default().clean(&dirty).to_string()
}

/// Version of the render pipeline. Part of the `render_cache` key: bump it
/// whenever `render_page` output changes for identical input.
pub const RENDERER_VERSION: i32 = 1;

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

/// Cache key hash. The title and locale render into the page, so they are
/// part of the key: correctness beats cross-page deduplication.
pub fn page_hash(title: &str, lang: &str, body_md: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(title.as_bytes());
    hasher.update([0u8]);
    hasher.update(lang.as_bytes());
    hasher.update([0u8]);
    hasher.update(body_md.as_bytes());
    hasher.finalize().to_vec()
}

/// Renders a full page through the `page.html` template.
pub fn render_page(
    env: &minijinja::Environment,
    title: &str,
    body_md: &str,
    wiki_name: &str,
    lang: &str,
) -> Result<RenderedPage, naw_core::error::AppError> {
    let content_hash = page_hash(title, lang, body_md);
    let body_html = render_html(body_md);
    let template = env.get_template("page.html").map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            title => title,
            wiki_name => wiki_name,
            lang => lang,
            body => body_html,
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
        assert!(html.contains("<h1>Title</h1>"));
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

    fn test_env() -> minijinja::Environment<'static> {
        naw_core::templates::load_templates(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skins/default"
        ))
        .expect("skins/default must exist with layout, page and 404 templates")
    }

    #[test]
    fn page_renders_title_and_body() {
        let html = render_page(&test_env(), "Home", "# Hi", "SnackersWIKI", "en")
            .expect("render")
            .html;
        assert!(html.contains("<title>Home"));
        assert!(html.contains("<h1>Hi</h1>"));
        assert!(html.contains("SnackersWIKI"));
    }

    #[test]
    fn page_hash_is_stable_and_sensitive() {
        let a = page_hash("T", "en", "# Hi");
        assert_eq!(a, page_hash("T", "en", "# Hi"));
        assert_ne!(a, page_hash("T", "en", "# Bye"));
        assert_ne!(a, page_hash("Other", "en", "# Hi"));
    }

    #[test]
    fn missing_template_is_an_error() {
        let env = minijinja::Environment::new();
        assert!(render_page(&env, "T", "x", "W", "en").is_err());
    }
}
