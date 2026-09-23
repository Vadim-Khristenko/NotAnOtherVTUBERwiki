//! NotAnotherWiki Engine Markdown render pipeline.
//!
//! Reader path contract: render on write, serve from cache. This function is
//! the pure core of that pipeline: Markdown in, sanitized HTML out.

/// Renders Markdown to sanitized HTML.
///
/// Chat-native set (v4): CommonMark plus tables, footnotes, task lists,
/// strikethrough (`~~`), superscript (`^`), subscript (`~`), math (`$`/`$$`),
/// GFM alerts (`> [!NOTE]`), definition lists, `[[wikilinks]]`, plus the
/// NotAnotherWiki sugar below. Raw HTML is disabled at the parser level
/// (decision D12), and the result is sanitized with ammonia regardless, so
/// every byte of output passed the whitelist.
///
/// Sugar, all server side in Rust so preview and save never disagree:
/// - `__italic__` renders as `<em>`, same as `*italic*` (per project order;
///   `**bold**` stays `<strong>`, underline uses `++` to avoid the
///   CommonMark/Discord `__` collision).
/// - `||spoiler||` becomes `<span class="spoiler">`, `==mark==` becomes
///   `<mark>`, `==red|text==` becomes `<mark class="mark-red">` (eight
///   fixed colors, unknown names stay literal), `++underline++` becomes
///   `<u>`, `((keys))` becomes `<kbd>`, `:fire:` shortcodes become glyphs.
/// - `> quote` is a blockquote, `>! Summary` plus `> body` lines is a
///   collapsible quote (`<details class="quote">`).
/// - `:::details Title ... :::` and `:::pullquote ... :::` blocks.
/// - `[[toc]]` alone in a paragraph becomes a nav of the page headings with
///   exact final anchors. Footnote definitions collect at the end of the
///   body no matter where their `[^n]:` lines stand.
///   `<mark>`, `++underline++` becomes `<u>`, `((keys))` becomes `<kbd>`,
///   and `:fire:` style shortcodes become Unicode pictographs from a fixed
///   table (unknown codes stay literal).
/// - `> quote` is a blockquote, `>! Summary` plus `> body` lines is a
///   collapsible quote (`<details class="quote">`).
/// - `:::details Title ... :::` and `:::pullquote ... :::` blocks.
/// - Fenced `mermaid`/`dot`/`graphviz`/`plantuml`/`math` keep their code text
///   and gain a class hook (`<pre class="mermaid">` etc) for the future
///   worker that renders them to SVG. No execution happens in Rust.
pub fn render_html(markdown: &str) -> String {
    render_html_with_depth(markdown, 0)
}

/// Recursion guard for nested custom blocks (`:::details` inside a
/// collapsible quote and the like). Depth 8 is far past sane authoring and
/// stops a malicious nesting chain from recursing on input size.
fn render_html_with_depth(markdown: &str, depth: usize) -> String {
    use pulldown_cmark::{Event, Options, Parser};

    if depth > 8 {
        return String::new();
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_SUPERSCRIPT);
    options.insert(Options::ENABLE_SUBSCRIPT);
    options.insert(Options::ENABLE_DEFINITION_LIST);
    options.insert(Options::ENABLE_WIKILINKS);

    // Block sugar first: `:::details`, `:::pullquote`, `>!` collapsible
    // quotes become placeholders that survive the parser as plain paragraphs.
    let (without_blocks, blocks) = extract_custom_blocks(markdown);
    // `__italic__` must reach pulldown-cmark as `*italic*`: the parser maps
    // `__` to `<strong>` and there is no flag to change that.
    let mapped = map_double_underscore_to_italic(&without_blocks);

    // Raw HTML is dropped here (decision D12). pulldown-cmark has no option
    // that refuses raw HTML, so the HTML events never reach the writer,
    // except a bare line break: `<br>` cannot execute anything.
    // ammonia is the second line of defence, not the first.
    let parser = Parser::new_ext(&mapped, options).filter(|event| match event {
        Event::Html(html) | Event::InlineHtml(html) => is_allowed_raw_html(html),
        _ => true,
    });

    let mut dirty = String::with_capacity(mapped.len());
    pulldown_cmark::html::push_html(&mut dirty, parser);
    // Block placeholders back to HTML. Inner bodies render through the same
    // pipeline (depth + 1), so nesting works and preview matches save.
    let dirty = restore_custom_blocks(&dirty, &blocks, depth);
    // Fenced diagrams keep text, gain a class hook for the worker.
    let dirty = postprocess_diagrams(&dirty);
    // Inline sugar on HTML text (inner formatting already rendered, so
    // `||**bold**||` keeps its `<strong>` inside the spoiler span). Code
    // and math sections are skipped.
    let mut dirty = postprocess_inline_spans(&dirty);
    // Tables: pulldown-cmark reports column alignment as an inline style,
    // which the sanitizer would strip. `align` survives the whitelist.
    dirty = preserve_table_alignment(&dirty);
    // Task list checkboxes are `<input>` elements, which the sanitizer
    // rightly drops. They become styled marks that keep their state as text.
    dirty = render_task_items(&dirty);
    // Footnotes move into the `fn-` namespace before heading ids are
    // assigned, so `# 1` and `[^1]` never share one anchor.
    dirty = namespace_footnote_ids(&dirty);
    let mut anchored = add_heading_ids(&dirty);
    // Last writer wins nothing: every id in the document must be unique, no
    // matter whether it came from a heading, a footnote or an author attr.
    anchored = dedupe_ids(&anchored);
    // Navigation and notes settle last: the table of contents needs final
    // anchors, footnotes belong at the bottom in reference order.
    anchored = insert_toc(&anchored);
    anchored = link_footnote_references(&anchored);
    anchored = collect_footnotes(&anchored);
    // `id` and `class` join the generic whitelist so heading anchors and
    // author styling survive. Neither executes anything; the worst a class
    // does is collide with site styles, which is the author's own choice.
    // `tabindex="0"` on spoilers keeps them keyboard and touch operable with
    // zero JavaScript on the reader path. `open` on `<details>` keeps
    // collapsible quotes working; `<ol start>` is already allowed by
    // ammonia's defaults.
    ammonia::Builder::default()
        .add_generic_attributes(["id", "class", "tabindex"])
        .add_tag_attributes("details", ["open"])
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

/// One extracted block: `:::details`, `:::pullquote`, or a `>!` collapsible
/// quote. The placeholder `NAWBLOCK{n}NAW` stands in for it while Markdown
/// runs, then renders back to fixed safe HTML.
struct CustomBlock {
    kind: BlockKind,
    title: String,
    body: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Details,
    Pullquote,
    CollapsibleQuote,
}

fn block_placeholder(idx: usize) -> String {
    format!("NAWBLOCK{idx}NAW")
}

/// Pulls `:::details` / `:::pullquote` fences and `>!` quote groups out of
/// the Markdown so the parser never sees their markers. Unclosed fences are
/// left literal so authors always see what they wrote.
fn extract_custom_blocks(markdown: &str) -> (String, Vec<CustomBlock>) {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut blocks: Vec<CustomBlock> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(rest) = trimmed
            .strip_prefix(":::details")
            .filter(|_| trimmed == ":::details" || trimmed.starts_with(":::details "))
        {
            let title = rest.trim().to_string();
            let mut body: Vec<&str> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim() != ":::" {
                body.push(lines[j]);
                j += 1;
            }
            if j >= lines.len() {
                out.push(lines[i].to_string());
                i += 1;
                continue;
            }
            let idx = blocks.len();
            blocks.push(CustomBlock {
                kind: BlockKind::Details,
                title,
                body: body.join("\n"),
            });
            out.push(String::new());
            out.push(block_placeholder(idx));
            out.push(String::new());
            i = j + 1;
            continue;
        }
        if trimmed == ":::pullquote" || trimmed.starts_with(":::pullquote ") {
            let mut body: Vec<&str> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim() != ":::" {
                body.push(lines[j]);
                j += 1;
            }
            if j >= lines.len() {
                out.push(lines[i].to_string());
                i += 1;
                continue;
            }
            let idx = blocks.len();
            blocks.push(CustomBlock {
                kind: BlockKind::Pullquote,
                title: String::new(),
                body: body.join("\n"),
            });
            out.push(String::new());
            out.push(block_placeholder(idx));
            out.push(String::new());
            i = j + 1;
            continue;
        }
        if let Some(summary) = parse_collapsible_opener(lines[i]) {
            let mut body: Vec<String> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                if parse_collapsible_opener(lines[j]).is_some() {
                    break;
                }
                match strip_quote_prefix(lines[j]) {
                    Some(content) => body.push(content.to_string()),
                    None => break,
                }
                j += 1;
            }
            let idx = blocks.len();
            blocks.push(CustomBlock {
                kind: BlockKind::CollapsibleQuote,
                title: summary,
                body: body.join("\n"),
            });
            out.push(String::new());
            out.push(block_placeholder(idx));
            out.push(String::new());
            i = j;
            continue;
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    (out.join("\n"), blocks)
}

/// `>! Summary` opens a collapsible quote. Up to three leading spaces match
/// CommonMark's blockquote rule; anything else is a normal paragraph.
fn parse_collapsible_opener(line: &str) -> Option<String> {
    let stripped = line.strip_prefix("   ").unwrap_or(line);
    let stripped = stripped.strip_prefix("  ").unwrap_or(stripped);
    let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
    let rest = stripped.strip_prefix('>')?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let summary = rest.strip_prefix('!')?;
    let summary = summary.strip_prefix(' ').unwrap_or(summary);
    Some(summary.trim_end().to_string())
}

/// Strips one `>` quote prefix. Returns `None` for non-quote lines, which
/// end a collapsible group.
fn strip_quote_prefix(line: &str) -> Option<&str> {
    let stripped = line.strip_prefix("   ").unwrap_or(line);
    let stripped = stripped.strip_prefix("  ").unwrap_or(stripped);
    let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
    let rest = stripped.strip_prefix('>')?;
    if let Some(bang) = rest.strip_prefix('!')
        && (bang.is_empty() || bang.starts_with(' '))
    {
        return None;
    }
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// Swaps block placeholders back for rendered HTML. Bodies render through
/// the same pipeline (depth + 1); titles stay plain escaped text so a
/// `<script>` in a summary never becomes markup.
fn restore_custom_blocks(html: &str, blocks: &[CustomBlock], depth: usize) -> String {
    let mut out = html.to_string();
    for (idx, block) in blocks.iter().enumerate() {
        let name = block_placeholder(idx);
        let rendered_body = if block.body.trim().is_empty() {
            String::new()
        } else {
            render_html_with_depth(&block.body, depth + 1)
        };
        let replacement = match block.kind {
            BlockKind::Details => {
                if rendered_body.is_empty() {
                    format!(
                        "<details class=\"details\"><summary>{}</summary></details>",
                        escape_html_text(&block.title)
                    )
                } else {
                    format!(
                        "<details class=\"details\"><summary>{}</summary>\n{rendered_body}\n</details>",
                        escape_html_text(&block.title)
                    )
                }
            }
            BlockKind::Pullquote => format!(
                "<figure class=\"pullquote\"><blockquote>\n{rendered_body}\n</blockquote></figure>"
            ),
            BlockKind::CollapsibleQuote => {
                if rendered_body.is_empty() {
                    format!(
                        "<details class=\"quote\"><summary>{}</summary></details>",
                        escape_html_text(&block.title)
                    )
                } else {
                    format!(
                        "<details class=\"quote\"><summary>{}</summary>\n<blockquote>\n{rendered_body}\n</blockquote>\n</details>",
                        escape_html_text(&block.title)
                    )
                }
            }
        };
        // The extractor emits the placeholder as its own paragraph, so only
        // the paragraph form is replaced: a literal NAWBLOCK0NAW inside a
        // code span never matches here.
        out = out.replace(&format!("<p>{name}</p>"), &replacement);
    }
    out
}

fn escape_html_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Maps `__italic__` to `*italic*` before parsing. pulldown-cmark hardwires
/// `__` to `<strong>`; the project order says double underscore is italic
/// and underline uses `++`, so this rewrite is the single place where that
/// rule lives. Fenced code, inline code, link destinations and `$` math are
/// left verbatim; `___triple___` is left to the parser (bold+italic).
fn map_double_underscore_to_italic(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_fence = false;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence {
            out.push_str(line);
            continue;
        }
        out.push_str(&map_underscores_in_line(line));
    }
    out
}

fn map_underscores_in_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut in_code = false;
    let mut in_math = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            in_code = !in_code;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '$' {
            in_math = !in_math;
            out.push(c);
            i += 1;
            continue;
        }
        if !in_code && !in_math && c == ']' && i + 1 < chars.len() && chars[i + 1] == '(' {
            out.push_str("](");
            i += 2;
            while i < chars.len() && chars[i] != ')' && chars[i] != '\n' {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if !in_code && !in_math && c == '_' && i + 1 < chars.len() && chars[i + 1] == '_' {
            let prev = if i == 0 { '\n' } else { chars[i - 1] };
            let next = if i + 2 >= chars.len() {
                '\n'
            } else {
                chars[i + 2]
            };
            if prev == '_' || next == '_' {
                out.push_str("__");
                i += 2;
                continue;
            }
            if let Some(end) = find_closing_double_underscore(&chars, i + 2) {
                let inner: String = chars[i + 2..end].iter().collect();
                if !inner.trim().is_empty() && !inner.contains("__") {
                    out.push('*');
                    out.push_str(&inner);
                    out.push('*');
                    i = end + 2;
                    continue;
                }
            }
            out.push_str("__");
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn find_closing_double_underscore(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '\n' {
            return None;
        }
        if chars[i] == '_' && chars[i + 1] == '_' {
            let prev = if i == 0 { '\n' } else { chars[i - 1] };
            let next_is_underscore = i + 2 < chars.len() && chars[i + 2] == '_';
            if prev != '_' && !next_is_underscore {
                return Some(i);
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    None
}

/// Fenced `mermaid`/`dot`/`graphviz`/`plantuml`/`math` blocks keep their text
/// and gain a class hook. The Bun worker (Phase 3) renders them to SVG later;
/// with no worker the reader still gets a readable code block. Degrade,
/// never fail.
fn postprocess_diagrams(html: &str) -> String {
    let mut out = html.to_string();
    for (lang, class) in [
        ("mermaid", "mermaid"),
        ("dot", "diagram diagram-dot"),
        ("graphviz", "diagram diagram-dot"),
        ("plantuml", "diagram diagram-plantuml"),
        ("puml", "diagram diagram-plantuml"),
        ("math", "math math-block"),
    ] {
        out = out.replace(
            &format!("<pre><code class=\"language-{lang}\">"),
            &format!("<pre class=\"{class}\"><code class=\"language-{lang}\">"),
        );
    }
    out
}

/// Inline chat sugar on rendered HTML: `||spoiler||`, `==mark==`,
/// `++underline++`, `((kbd))` and `:emoji:` shortcodes. Runs on HTML (not
/// Markdown) so inner `**bold**` is
/// already `<strong>` and survives inside the wrapper. `<pre>`, `<code>`
/// and math spans are skipped so code samples stay literal.
fn postprocess_inline_spans(html: &str) -> String {
    let mut current = html.to_string();
    for pass in [
        Pass::Delimited("||", "<span class=\"spoiler\" tabindex=\"0\">", "</span>"),
        Pass::MarkColor,
        Pass::Delimited("==", "<mark>", "</mark>"),
        Pass::Delimited("++", "<u>", "</u>"),
        Pass::Kbd,
        Pass::Emoji,
    ] {
        let segments = split_protected_html(&current);
        let mut next = String::with_capacity(current.len());
        for seg in segments {
            if seg.protected {
                next.push_str(seg.text);
            } else {
                match pass {
                    Pass::Delimited(d, open, close) => {
                        next.push_str(&replace_delimited(seg.text, d, open, close));
                    }
                    Pass::MarkColor => next.push_str(&replace_mark_color(seg.text)),
                    Pass::Kbd => next.push_str(&replace_kbd(seg.text)),
                    Pass::Emoji => next.push_str(&replace_emoji(seg.text)),
                }
            }
        }
        current = next;
    }
    current
}

#[derive(Clone, Copy)]
enum Pass {
    Delimited(&'static str, &'static str, &'static str),
    MarkColor,
    Kbd,
    Emoji,
}

/// Fixed highlight colors for `==color|text==`. A closed set keeps author
/// input out of both class names and style attributes.
const MARK_COLORS: &[&str] = &[
    "red", "orange", "yellow", "green", "blue", "violet", "pink", "gray",
];

/// `==red|text==` becomes `<mark class="mark-red">`. Unknown color names
/// fall through to the plain `==mark==` pass, so `==a|b==` still highlights
/// instead of dying. HTML tags are skipped.
fn replace_mark_color(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let d = ['=', '='];
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if matches_delim(&chars, i, &d) && is_valid_opener(&chars, i, d.len()) {
            let mut j = i + 2;
            while j < chars.len() && chars[j].is_ascii_lowercase() {
                j += 1;
            }
            let name: String = chars[i + 2..j].iter().collect();
            let piped = j < chars.len() && chars[j] == '|';
            if piped
                && MARK_COLORS.contains(&name.as_str())
                && let Some(end) = find_valid_closer(&chars, j + 1, &d)
            {
                let inner: String = chars[j + 1..end].iter().collect();
                if !inner.trim().is_empty() && !inner.contains("==") {
                    out.push_str(&format!("<mark class=\"mark-{name}\">"));
                    out.push_str(&inner);
                    out.push_str("</mark>");
                    i = end + 2;
                    continue;
                }
            }
            out.push_str("==");
            i += 2;
            continue;
        }
        if chars[i] == '<' {
            while i < chars.len() && chars[i] != '>' {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                out.push('>');
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

struct HtmlSegment<'a> {
    text: &'a str,
    protected: bool,
}

/// Splits rendered HTML into protected (`<pre>`, `<code>`, math spans) and
/// normal segments. Delimiter replacement only runs on normal ones.
fn split_protected_html(html: &str) -> Vec<HtmlSegment<'_>> {
    let mut segs = Vec::new();
    let mut i = 0;
    let mut normal_start = 0;
    while i < html.len() {
        let rest = &html[i..];
        let is_guard = rest.starts_with("<pre")
            || rest.starts_with("<code")
            || (rest.starts_with("<span") && rest.get(..64).unwrap_or("").contains("math"));
        let guard: Option<usize> = if rest.starts_with("<pre") {
            html[i..].find("</pre>").map(|p| p + 6)
        } else if rest.starts_with("<code") {
            html[i..].find("</code>").map(|p| p + 7)
        } else if is_guard {
            html[i..].find("</span>").map(|p| p + 7)
        } else {
            None
        };
        if is_guard {
            if i > normal_start {
                segs.push(HtmlSegment {
                    text: &html[normal_start..i],
                    protected: false,
                });
            }
            let len = guard.unwrap_or(rest.len());
            segs.push(HtmlSegment {
                text: &html[i..i + len],
                protected: true,
            });
            i += len;
            normal_start = i;
        } else {
            // One character, not one byte: `&html[i..]` above must always land
            // on a character boundary, and Cyrillic letters are two bytes.
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    if normal_start < html.len() {
        segs.push(HtmlSegment {
            text: &html[normal_start..],
            protected: false,
        });
    }
    segs
}

/// Replaces `||a||`-style pairs with an HTML wrapper. Flanking rules keep
/// `C++`, `x==y` and `a||b` literal: the opener needs a non-alphanumeric
/// (or start) before it and a non-space after it; the closer needs a
/// non-space before it and a non-alphanumeric (or end) after it. First
/// close wins, empty pairs are left alone.
fn replace_delimited(text: &str, delim: &str, open_tag: &str, close_tag: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let d: Vec<char> = delim.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if matches_delim(&chars, i, &d) && is_valid_opener(&chars, i, d.len()) {
            if let Some(end) = find_valid_closer(&chars, i + d.len(), &d) {
                let inner: String = chars[i + d.len()..end].iter().collect();
                if !inner.trim().is_empty() && !inner.contains(delim) {
                    out.push_str(open_tag);
                    out.push_str(&inner);
                    out.push_str(close_tag);
                    i = end + d.len();
                    continue;
                }
            }
            for _ in 0..d.len() {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        // Skip over HTML tags so `class="a==b"` never matches.
        if chars[i] == '<' {
            while i < chars.len() && chars[i] != '>' {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                out.push('>');
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn matches_delim(chars: &[char], at: usize, d: &[char]) -> bool {
    at + d.len() <= chars.len() && chars[at..at + d.len()] == *d
}

fn is_valid_opener(chars: &[char], at: usize, len: usize) -> bool {
    let prev_ok = at == 0 || !chars[at - 1].is_alphanumeric();
    let next_ok = at + len < chars.len() && !chars[at + len].is_whitespace();
    prev_ok && next_ok
}

fn find_valid_closer(chars: &[char], from: usize, d: &[char]) -> Option<usize> {
    let mut i = from;
    while i + d.len() <= chars.len() {
        if chars[i] == '\n' {
            return None;
        }
        if chars[i] == '<' {
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            i += 1;
            continue;
        }
        if matches_delim(chars, i, d) {
            let prev_ok = i > 0 && !chars[i - 1].is_whitespace();
            let next_ok = i + d.len() >= chars.len() || !chars[i + d.len()].is_alphanumeric();
            if prev_ok && next_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// `((Ctrl+C))` becomes `<kbd>Ctrl+C</kbd>`. Same flanking idea as the
/// symmetric delimiters, with `(` and `)` as the guard characters so smileys
/// like `:((` stay literal. HTML tags are skipped.
fn replace_kbd(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let opens = i + 1 < chars.len() && chars[i] == '(' && chars[i + 1] == '(';
        let prev_ok = i == 0 || chars[i - 1] != '(';
        let next_ok = i + 2 < chars.len() && !chars[i + 2].is_whitespace();
        if opens && prev_ok && next_ok {
            if let Some(end) = find_kbd_close(&chars, i + 2) {
                let inner: String = chars[i + 2..end].iter().collect();
                if !inner.trim().is_empty() && !inner.contains("((") && !inner.contains("))") {
                    out.push_str("<kbd>");
                    out.push_str(&inner);
                    out.push_str("</kbd>");
                    i = end + 2;
                    continue;
                }
            }
            out.push_str("((");
            i += 2;
            continue;
        }
        if chars[i] == '<' {
            while i < chars.len() && chars[i] != '>' {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                out.push('>');
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn find_kbd_close(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '\n' {
            return None;
        }
        if chars[i] == '<' {
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            i += 1;
            continue;
        }
        if chars[i] == ')' && chars[i + 1] == ')' {
            let prev_ok = i > 0 && !chars[i - 1].is_whitespace();
            let next_ok = i + 2 >= chars.len() || chars[i + 2] != ')';
            if prev_ok && next_ok {
                return Some(i);
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    None
}

/// `:fire:` style shortcodes become Unicode pictographs from a fixed table.
/// Unknown codes, times like `12:30` and URLs stay literal: the opener needs
/// a non-alphanumeric before it and an alphanumeric after it, the name is
/// 2 to 32 chars from a small charset, and the closer must not be followed
/// by an alphanumeric. HTML tags are skipped.
fn replace_emoji(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            while i < chars.len() && chars[i] != '>' {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                out.push('>');
                i += 1;
            }
            continue;
        }
        if chars[i] == ':'
            && (i == 0 || (!chars[i - 1].is_alphanumeric() && chars[i - 1] != ':'))
            && i + 1 < chars.len()
            && chars[i + 1].is_alphanumeric()
        {
            let mut j = i + 1;
            while j < chars.len() && is_shortcode_char(chars[j]) {
                j += 1;
            }
            let name: String = chars[i + 1..j].iter().collect();
            let closed = j < chars.len() && chars[j] == ':';
            let next_ok = j + 1 >= chars.len() || !chars[j + 1].is_alphanumeric();
            if closed
                && next_ok
                && let Some(glyph) = emoji_for(&name)
            {
                out.push_str(glyph);
                i = j + 1;
                continue;
            }
            out.push(':');
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_shortcode_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+'
}

/// Fixed shortcode table, Discord and GitHub flavored. Deliberately small:
/// every entry is plain Unicode text, so it works with JavaScript off and in
/// every skin with no assets. Unknown codes render literally.
fn emoji_for(name: &str) -> Option<&'static str> {
    Some(match name {
        "heart" => "\u{2764}",
        "fire" => "\u{1F525}",
        "star" => "\u{2B50}",
        "sparkles" => "\u{2728}",
        "tada" | "party" => "\u{1F389}",
        "rocket" => "\u{1F680}",
        "eyes" => "\u{1F440}",
        "wave" => "\u{1F44B}",
        "grin" => "\u{1F601}",
        "smile" => "\u{1F604}",
        "laugh" | "joy" => "\u{1F602}",
        "wink" => "\u{1F609}",
        "blush" => "\u{1F60A}",
        "heart_eyes" => "\u{1F60D}",
        "star_struck" => "\u{1F929}",
        "cry" => "\u{1F622}",
        "sob" => "\u{1F62D}",
        "angry" => "\u{1F620}",
        "thinking" => "\u{1F914}",
        "hug" => "\u{1F917}",
        "sleep" => "\u{1F634}",
        "clown" => "\u{1F921}",
        "ghost" => "\u{1F47B}",
        "alien" => "\u{1F47D}",
        "robot" => "\u{1F916}",
        "cat" => "\u{1F431}",
        "dog" => "\u{1F436}",
        "fox" => "\u{1F98A}",
        "frog" => "\u{1F438}",
        "panda" => "\u{1F43C}",
        "penguin" => "\u{1F427}",
        "unicorn" => "\u{1F984}",
        "dragon" => "\u{1F432}",
        "thumbsup" | "+1" => "\u{1F44D}",
        "thumbsdown" | "-1" => "\u{1F44E}",
        "ok" => "\u{1F44C}",
        "point_right" => "\u{1F449}",
        "clap" => "\u{1F44F}",
        "pray" => "\u{1F64F}",
        "muscle" => "\u{1F4AA}",
        "crown" => "\u{1F451}",
        "gem" => "\u{1F48E}",
        "trophy" => "\u{1F3C6}",
        "medal" => "\u{1F3C5}",
        "bell" => "\u{1F514}",
        "gift" => "\u{1F381}",
        "cake" => "\u{1F382}",
        "cookie" => "\u{1F36A}",
        "donut" => "\u{1F369}",
        "popcorn" => "\u{1F37F}",
        "pizza" => "\u{1F355}",
        "burger" => "\u{1F354}",
        "fries" => "\u{1F35F}",
        "taco" => "\u{1F32E}",
        "sushi" => "\u{1F363}",
        "ramen" => "\u{1F35C}",
        "cheese" => "\u{1F9C0}",
        "egg" => "\u{1F373}",
        "strawberry" => "\u{1F353}",
        "peach" => "\u{1F351}",
        "grape" => "\u{1F347}",
        "melon" => "\u{1F348}",
        "coffee" => "\u{2615}",
        "tea" => "\u{1F375}",
        "game" => "\u{1F3AE}",
        "dice" => "\u{1F3B2}",
        "mic" => "\u{1F3A4}",
        "headphones" => "\u{1F3A7}",
        "music" => "\u{1F3B5}",
        "movie" => "\u{1F3AC}",
        "camera" => "\u{1F4F7}",
        "book" => "\u{1F4DA}",
        "pin" => "\u{1F4CC}",
        "check" => "\u{2705}",
        "cross" => "\u{274C}",
        "warn" => "\u{26A0}\u{FE0F}",
        "info" => "\u{2139}\u{FE0F}",
        "question" => "\u{2753}",
        "bang" => "\u{2757}",
        "plus" => "\u{2795}",
        "minus" => "\u{2796}",
        "arrow_right" => "\u{27A1}\u{FE0F}",
        "recycle" => "\u{267B}\u{FE0F}",
        "100" => "\u{1F4AF}",
        "vs" => "\u{1F19A}",
        _ => return None,
    })
}

/// pulldown-cmark marks aligned table cells with an inline `style`, which
/// ammonia strips. Only the table writer emits this exact shape, so a plain
/// rewrite to the whitelisted `align` attribute is safe.
fn preserve_table_alignment(html: &str) -> String {
    let mut out = html.to_string();
    for align in ["left", "center", "right"] {
        out = out.replace(
            &format!(" style=\"text-align: {align}\">"),
            &format!(" align=\"{align}\">"),
        );
    }
    out
}

/// Moves footnote anchors into the `fn-` namespace: `[^a]` becomes
/// `#fn-a` on both the reference and the definition. Exact prefixes from
/// the pulldown-cmark writer, so author text can never collide with them.
fn namespace_footnote_ids(html: &str) -> String {
    html.replace(
        "<div class=\"footnote-definition\" id=\"",
        "<div class=\"footnote-definition\" id=\"fn-",
    )
    .replace(
        "<sup class=\"footnote-reference\"><a href=\"#",
        "<sup class=\"footnote-reference\"><a href=\"#fn-",
    )
}

/// Enforces document wide id uniqueness in document order. The first use of
/// an id wins; later ones gain `-2`, `-3` suffixes. Renamed footnote
/// definitions pull their reference links along, so jumps never land on the
/// wrong element. Plain content links to a duplicated anchor keep pointing
/// at the first one, which matches the heading rule authors already know.
fn dedupe_ids(html: &str) -> String {
    use std::collections::HashMap;
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut renames: Vec<(String, String)> = Vec::new();
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(pos) = rest.find(" id=\"") {
        let val_start = pos + 5;
        let Some(end) = rest[val_start..].find('"') else {
            break;
        };
        let id = &rest[val_start..val_start + end];
        let count = seen.entry(id.to_string()).or_insert(0);
        *count += 1;
        if *count == 1 {
            out.push_str(&rest[..val_start + end]);
        } else {
            let new_id = format!("{}-{}", id, count);
            out.push_str(&rest[..val_start]);
            out.push_str(&new_id);
            let context = &rest[..pos];
            let tail = if context.len() > 64 {
                &context[context.len() - 64..]
            } else {
                context
            };
            if tail.contains("<div class=\"footnote-definition\"") {
                renames.push((id.to_string(), new_id));
            }
        }
        rest = &rest[val_start + end..];
    }
    out.push_str(rest);
    for (old, new) in renames {
        out = out.replace(
            &format!("<sup class=\"footnote-reference\"><a href=\"#{old}\">"),
            &format!("<sup class=\"footnote-reference\"><a href=\"#{new}\">"),
        );
    }
    out
}
/// Replaces a lone `[[toc]]` paragraph with a nav of the page headings.
/// Runs on final HTML so links match the assigned ids exactly, duplicates
/// included. Accepts the raw `[[toc]]` form and the wikilink form the
/// parser makes of it. Without headings the placeholder vanishes silently.
fn insert_toc(html: &str) -> String {
    let mut items: Vec<(u32, String, String)> = Vec::new();
    let mut rest = html;
    while let Some(open) = rest.find("<h") {
        let level = rest[open + 2..].chars().next();
        let Some(n) = level
            .and_then(|c| c.to_digit(10))
            .filter(|n| (1..=6).contains(n))
        else {
            rest = &rest[open + 2..];
            continue;
        };
        let tag_start = open + 3;
        let Some(tag_end) = rest[tag_start..].find('>') else {
            break;
        };
        let tag = &rest[tag_start..tag_start + tag_end];
        let Some(id) = attr_value(tag, "id") else {
            rest = &rest[tag_start + tag_end + 1..];
            continue;
        };
        let content_start = tag_start + tag_end + 1;
        let close = format!("</h{n}>");
        let Some(content_end) = rest[content_start..].find(&close) else {
            break;
        };
        let inner = &rest[content_start..content_start + content_end];
        // Already HTML escaped by the renderer, so no second escaping here.
        let text = strip_inline_tags(inner).trim().to_string();
        if !text.is_empty() {
            items.push((n, id, text));
        }
        rest = &rest[content_start + content_end + close.len()..];
    }
    // A lone `#` heading is the page title, already on screen above the
    // contents. Only when an author uses several does level one mean sections.
    if items.iter().filter(|(level, _, _)| *level == 1).count() == 1 {
        items.retain(|(level, _, _)| *level != 1);
    }
    let nav = if items.is_empty() {
        String::new()
    } else {
        let mut nav = String::from("<nav class=\"toc\"><ul>");
        for (level, id, text) in &items {
            nav.push_str(&format!(
                "<li class=\"toc-{level}\"><a href=\"#{id}\">{text}</a></li>"
            ));
        }
        nav.push_str("</ul></nav>");
        nav
    };
    html.replace("<p>[[toc]]</p>", &nav)
        .replace("<p><a href=\"toc\">toc</a></p>", &nav)
}

/// Reads `attr="value"` from a tag fragment. First match wins.
fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let key = format!("{attr}=\"");
    let pos = tag.find(&key)?;
    let start = pos + key.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

/// Collects footnote definitions at the end of the body. pulldown-cmark
/// emits each definition where its `[^n]:` line stands, so a note defined
/// mid article would split the reading flow. Readers expect notes at the
/// bottom in reference order, so they move into one closing block. Nesting
/// aware: a definition holding details or divs keeps them.
fn collect_footnotes(html: &str) -> String {
    const OPEN: &str = "<div class=\"footnote-definition\"";
    let mut defs: Vec<&str> = Vec::new();
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(pos) = rest.find(OPEN) {
        out.push_str(&rest[..pos]);
        let mut depth = 0;
        let mut i = pos;
        let mut end = None;
        while i < rest.len() {
            if rest[i..].starts_with("</div>") {
                depth -= 1;
                i += 6;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
                continue;
            }
            if rest[i..].starts_with("<div")
                && rest[i + 4..]
                    .chars()
                    .next()
                    .map(|c| c == ' ' || c == '>' || c == '/' || c == '\t' || c == '\n')
                    .unwrap_or(false)
            {
                depth += 1;
            }
            // Step a whole character, so the slices above stay on boundaries
            // when a note is written in anything but ASCII.
            i += rest[i..].chars().next().map_or(1, char::len_utf8);
        }
        match end {
            Some(stop) => {
                defs.push(&rest[pos..stop]);
                rest = &rest[stop..];
            }
            None => {
                out.push_str(&rest[pos..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    if defs.is_empty() {
        return out;
    }
    out.push_str("<div class=\"footnotes\">");
    for def in defs {
        // A note whose reference got an anchor links back to it, so a reader
        // who jumped down can return to the sentence they left.
        let back = attr_value(def, "id")
            .and_then(|id| id.strip_prefix("fn-").map(|n| format!("fnref-{n}")))
            .filter(|back| {
                out.contains(&format!("<sup class=\"footnote-reference\" id=\"{back}\">"))
            });
        match (back, def.strip_suffix("</div>")) {
            (Some(back), Some(body)) => {
                out.push_str(body);
                out.push_str(&format!(
                    "<a class=\"footnote-backref\" href=\"#{back}\">\u{21A9}\u{FE0E}</a></div>"
                ));
            }
            _ => out.push_str(def),
        }
    }
    out.push_str("</div>");
    out
}

/// Gives the first reference to each footnote an anchor (`fnref-1` for
/// `fn-1`), the target of the note's way back. Later references to the same
/// note stay plain: one note can only return to one place. An id already in
/// use elsewhere wins, and that note simply goes without a way back.
fn link_footnote_references(html: &str) -> String {
    const OPEN: &str = "<sup class=\"footnote-reference\"><a href=\"#fn-";
    let mut out = String::with_capacity(html.len() + 64);
    let mut linked = std::collections::HashSet::new();
    let mut rest = html;
    while let Some(pos) = rest.find(OPEN) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + OPEN.len()..];
        let Some(quote) = after.find('"') else {
            break;
        };
        let label = &after[..quote];
        let anchor = format!("fnref-{label}");
        let taken = html.contains(&format!("id=\"{anchor}\""));
        if !taken && linked.insert(label.to_string()) {
            out.push_str(&format!(
                "<sup class=\"footnote-reference\" id=\"{anchor}\">"
            ));
        } else {
            out.push_str("<sup class=\"footnote-reference\">");
        }
        out.push_str("<a href=\"#fn-");
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Turns pulldown-cmark's disabled checkboxes into marks the sanitizer
/// keeps. The state stays in the text as `[x]` or `[ ]` for screen readers
/// and copy paste, and skins draw the box from the `task-done` class.
fn render_task_items(html: &str) -> String {
    const DONE: &str = "<li><input disabled=\"\" type=\"checkbox\" checked=\"\"/>\n";
    const TODO: &str = "<li><input disabled=\"\" type=\"checkbox\"/>\n";
    html.replace(
        DONE,
        "<li class=\"task task-done\"><span class=\"task-mark\">[x]</span> ",
    )
    .replace(
        TODO,
        "<li class=\"task\"><span class=\"task-mark\">[ ]</span> ",
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
/// whenever `render_body` output changes for identical input.
///
/// Skin and chrome changes no longer count. The cache holds the body fragment
/// only, so a footer edit is not a new rendering, and the same article under
/// two skins is one cache row instead of two.
pub const RENDERER_VERSION: i32 = 12;

/// A rendered body fragment plus the key it is cached under.
pub struct RenderedBody {
    pub content_hash: Vec<u8>,
    pub html: String,
    /// How long the Markdown stage took. The footer shows it, and it is the
    /// only part of the page worth measuring: the shell is a template render.
    pub render_ms: u64,
}

/// Body-only content hash, and the `render_cache` key.
///
/// The same value `revisions.content_hash` stores, which is the point: a
/// revision and its cached rendering are addressed by one hash, so looking up
/// the rendering for a revision needs no second column and cannot disagree.
pub fn content_hash(body_md: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(body_md.as_bytes()).to_vec()
}

/// Renders Markdown to a sanitized HTML fragment, timed, with its cache key.
///
/// This is the expensive half of serving a page and the only half worth
/// caching. Assembling the document around it belongs to the web layer,
/// because the surrounding chrome depends on who is asking and must never end
/// up in a shared cache row.
pub fn render_body(body_md: &str) -> RenderedBody {
    let started = std::time::Instant::now();
    let html = render_html(body_md);
    RenderedBody {
        content_hash: content_hash(body_md),
        html,
        render_ms: started.elapsed().as_millis() as u64,
    }
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
        assert!(html.contains("href=\"#fn-1\""));
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

    #[test]
    fn render_body_returns_the_fragment_and_its_key() {
        let rendered = render_body("# Hi");
        assert_eq!(rendered.html, render_html("# Hi"));
        assert!(rendered.html.contains("<h1 id=\"hi\">Hi</h1>"));
        // A fragment, not a document: assembling one is the web layer's job.
        assert!(!rendered.html.contains("<html"));
        assert!(!rendered.html.contains("<title"));
        assert_eq!(rendered.content_hash, content_hash("# Hi"));
    }

    #[test]
    fn the_cache_key_is_the_revision_hash_and_nothing_else() {
        // The design claim of the split render, asserted. The cache holds the
        // body fragment, so nothing outside the body may change the key: the
        // same article under two skins, two titles or two locales is one cache
        // row, and editing only the title does not throw the rendering away.
        let a = content_hash("# Hi");
        assert_eq!(a, content_hash("# Hi"));
        assert_eq!(a, render_body("# Hi").content_hash);
        assert_ne!(a, content_hash("# Bye"));
        // Whitespace is content: a trailing newline is a different revision.
        assert_ne!(a, content_hash("# Hi\n"));
    }

    #[test]
    fn double_underscore_is_italic() {
        let html = render_html("__Filian__ is fast.");
        assert!(html.contains("<em>Filian</em>"), "{html}");
        assert!(!html.contains("<strong>Filian</strong>"));
    }

    #[test]
    fn strikethrough_sup_sub_render() {
        // pulldown-cmark uses flanking rules: intra-word `H~2~O` stays
        // literal, spaced delimiters render. Document the spaced form.
        let html = render_html("~~gone~~ H ~2~ O E=mc ^2^.\n");
        assert!(html.contains("<del>gone</del>"), "{html}");
        assert!(html.contains("<sub>2</sub>"), "{html}");
        assert!(html.contains("<sup>2</sup>"), "{html}");
    }

    #[test]
    fn spoiler_mark_underline_render() {
        let html = render_html("||secret|| ==lit== ++under++.\n");
        assert!(
            html.contains("<span class=\"spoiler\" tabindex=\"0\">secret</span>"),
            "{html}"
        );
        assert!(html.contains("<mark>lit</mark>"), "{html}");
        assert!(html.contains("<u>under</u>"), "{html}");
    }

    #[test]
    fn spoiler_keeps_inner_bold() {
        let html = render_html("||**bold** inside||\n");
        assert!(html.contains("<span class=\"spoiler\""), "{html}");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
    }

    #[test]
    fn code_protects_sugar() {
        let html = render_html("`||not spoiler||`\n");
        assert!(!html.contains("spoiler\">"), "{html}");
        assert!(html.contains("||not spoiler||"), "{html}");
    }

    #[test]
    fn math_renders_with_class() {
        let html = render_html("Einstein: $E=mc^2$\n\n$$x+y$$\n");
        assert!(html.contains("math-inline"), "{html}");
        assert!(html.contains("math-display"), "{html}");
    }

    #[test]
    fn gfm_alert_renders() {
        let html = render_html("> [!NOTE]\n> Keep it cozy.\n");
        assert!(html.contains("markdown-alert-note"), "{html}");
    }

    #[test]
    fn collapsible_quote_renders() {
        let html = render_html(">! Click me\n> hidden text\n");
        assert!(html.contains("<details class=\"quote\">"), "{html}");
        assert!(html.contains("<summary>Click me</summary>"), "{html}");
        assert!(html.contains("hidden text"), "{html}");
    }

    #[test]
    fn details_block_renders() {
        let html = render_html(":::details Lore\nBody **bold**.\n:::\n");
        assert!(html.contains("<details class=\"details\">"), "{html}");
        assert!(html.contains("<summary>Lore</summary>"), "{html}");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
    }

    #[test]
    fn pullquote_block_renders() {
        let html = render_html(":::pullquote\nShe is fast.\n:::\n");
        assert!(html.contains("<figure class=\"pullquote\">"), "{html}");
    }

    #[test]
    fn mermaid_fence_keeps_text_with_hook() {
        let html = render_html("```mermaid\ngraph TD;\n```\n");
        assert!(html.contains("<pre class=\"mermaid\">"), "{html}");
        assert!(html.contains("graph TD;"), "{html}");
    }

    #[test]
    fn wikilink_renders() {
        let html = render_html("See [[home|Home page]].\n");
        assert!(html.contains("href=\"home\""), "{html}");
        assert!(html.contains(">Home page</a>"), "{html}");
    }

    #[test]
    fn definition_list_renders() {
        let html = render_html("Term\n  : Definition.\n");
        assert!(html.contains("<dl>"), "{html}");
        assert!(html.contains("<dt>"), "{html}");
    }

    #[test]
    fn kbd_renders() {
        let html = render_html("Press ((Ctrl+C)) to copy.\n");
        assert!(html.contains("<kbd>Ctrl+C</kbd>"), "{html}");
    }

    #[test]
    fn kbd_skipped_in_code() {
        let html = render_html("`((not a key))`\n");
        assert!(!html.contains("<kbd>"), "{html}");
    }

    #[test]
    fn emoji_shortcodes_render() {
        let html = render_html("Good luck :fire: and :tada:!\n");
        assert!(html.contains("\u{1F525}"), "{html}");
        assert!(html.contains("\u{1F389}"), "{html}");
        assert!(!html.contains(":fire:"), "{html}");
    }

    #[test]
    fn heading_one_and_footnote_one_share_no_anchor() {
        let html = render_html("# 1\n\nText[^1].\n\n[^1]: The note.\n");
        assert!(html.contains("<h1 id=\"1\">1</h1>"), "{html}");
        assert!(html.contains("id=\"fn-1\""), "{html}");
        assert!(html.contains("href=\"#fn-1\""), "{html}");
        assert_eq!(html.matches("id=\"1\"").count(), 1);
    }

    #[test]
    fn author_id_colliding_with_footnote_gets_suffixed_with_refs_following() {
        let html = render_html("## Taken {id=\"fn-1\"}\n\nText[^1].\n\n[^1]: The note.\n");
        assert!(html.contains("<h2 id=\"fn-1\">Taken</h2>"), "{html}");
        assert!(html.contains("id=\"fn-1-2\""), "{html}");
        assert!(html.contains("href=\"#fn-1-2\""), "{html}");
    }

    #[test]
    fn footnotes_collect_at_the_bottom() {
        let html = render_html("Text[^1].\n\n[^1]: The note.\n\nAfter.\n");
        let note = html.find("The note.").expect("note renders");
        let after = html.find("After.").expect("tail renders");
        assert!(after < note, "{html}");
        assert!(html.contains("<div class=\"footnotes\">"), "{html}");
    }

    #[test]
    fn colored_marks_render_and_unknown_names_fall_through() {
        let html = render_html("==red|hot== and ==plain== and ==nope|x==.\n");
        assert!(
            html.contains("<mark class=\"mark-red\">hot</mark>"),
            "{html}"
        );
        assert!(html.contains("<mark>plain</mark>"), "{html}");
        assert!(html.contains("<mark>nope|x</mark>"), "{html}");
    }

    #[test]
    fn toc_lists_headings_with_final_anchors() {
        let html = render_html("[[toc]]\n\n## One\n\n### One\n");
        assert!(html.contains("<nav class=\"toc\">"), "{html}");
        assert!(html.contains("href=\"#one\""), "{html}");
        assert!(html.contains("href=\"#one-2\""), "{html}");
    }

    #[test]
    fn toc_leaves_out_a_lone_title_but_keeps_several() {
        let html = render_html("# Title\n\n[[toc]]\n\n## Part\n");
        assert!(!html.contains("href=\"#title\""), "{html}");
        assert!(html.contains("href=\"#part\""), "{html}");
        let html = render_html("[[toc]]\n\n# One\n\n# Two\n");
        assert!(html.contains("href=\"#one\""), "{html}");
        assert!(html.contains("href=\"#two\""), "{html}");
    }

    #[test]
    fn task_items_keep_their_state_without_inputs() {
        let html = render_html("- [x] Done\n- [ ] Todo\n");
        assert!(!html.contains("<input"), "{html}");
        assert!(
            html.contains("<li class=\"task task-done\"><span class=\"task-mark\">[x]</span> Done"),
            "{html}"
        );
        assert!(
            html.contains("<li class=\"task\"><span class=\"task-mark\">[ ]</span> Todo"),
            "{html}"
        );
    }

    #[test]
    fn footnotes_link_back_to_their_first_reference() {
        let html = render_html("One[^a] and again[^a].\n\n[^a]: The note.\n");
        assert_eq!(html.matches("id=\"fnref-a\"").count(), 1, "{html}");
        assert!(html.contains("href=\"#fnref-a\""), "{html}");
        let note = html.find("id=\"fn-a\"").expect("note renders");
        let back = html.find("footnote-backref").expect("way back renders");
        assert!(note < back, "{html}");
    }

    #[test]
    fn footnote_backref_yields_to_a_taken_id() {
        let html = render_html("# fnref-a\n\nText[^a].\n\n[^a]: The note.\n");
        assert_eq!(html.matches("id=\"fnref-a\"").count(), 1, "{html}");
        assert!(!html.contains("footnote-backref"), "{html}");
    }

    #[test]
    fn toc_without_headings_vanishes() {
        let html = render_html("[[toc]]\n\nJust text.\n");
        assert!(!html.contains("toc"), "{html}");
    }

    #[test]
    fn table_alignment_survives_sanitizing() {
        let html = render_html("| l | c | r |\n|:--|:--:|--:|\n| 1 | 2 | 3 |\n");
        assert!(html.contains("align=\"left\""), "{html}");
        assert!(html.contains("align=\"center\""), "{html}");
        assert!(html.contains("align=\"right\""), "{html}");
        assert!(!html.contains("text-align"), "{html}");
    }

    #[test]
    fn emoji_leaves_times_urls_and_unknown_alone() {
        let html = render_html("Meet at 12:30, see https://x.test/a and :nope:.\n");
        assert!(html.contains("12:30"), "{html}");
        assert!(html.contains("https://x.test/a"), "{html}");
        assert!(html.contains(":nope:"), "{html}");
    }

    #[test]
    fn details_summary_survives_sanitize() {
        let html = render_html(">! Title\n> body\n");
        assert!(html.contains("<details"), "{html}");
        assert!(html.contains("<summary>"), "{html}");
    }
}

#[cfg(test)]
mod non_ascii {
    //! The HTML post-processing walks strings by position. Every walk must step
    //! by whole characters: a byte step lands inside a two-byte Cyrillic letter
    //! and panics, which took down saving any Russian page with a heading.
    use super::render_html;

    #[test]
    fn cyrillic_around_headings_and_emphasis() {
        let html = render_html(
            "## Обо мне

Тестирую **альфу**.",
        );
        assert!(html.contains("Обо мне"), "{html}");
        assert!(html.contains("<strong>альфу</strong>"), "{html}");
    }

    #[test]
    fn non_ascii_next_to_protected_spans() {
        let html = render_html("Код: `пример` и дальше ==метка== и $x$ тоже.");
        assert!(html.contains("<code>пример</code>"), "{html}");
        assert!(html.contains("<mark>метка</mark>"), "{html}");
    }

    #[test]
    fn non_ascii_footnotes() {
        let html = render_html(
            "Текст[^1].

[^1]: Примечание на русском, 日本語 тоже.
",
        );
        assert!(html.contains("Примечание на русском"), "{html}");
        assert!(html.contains("footnote-backref"), "{html}");
    }

    #[test]
    fn emoji_and_rtl_survive() {
        let html = render_html(
            "# 🍓 Заголовок

مرحبا **بالعالم** 👋",
        );
        assert!(html.contains("بالعالم"), "{html}");
        assert!(html.contains("🍓"), "{html}");
    }
}
