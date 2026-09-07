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
}
