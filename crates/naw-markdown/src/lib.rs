//! Markdown to sanitized HTML, the pure core of render-on-write.

mod scan;
pub mod transclude;

use std::collections::HashSet;

use scan::{Closers, Counts};

/// Renders Markdown to sanitized HTML.
///
/// CommonMark plus tables, footnotes, task lists, strikethrough, super- and
/// subscript, math, GFM alerts, definition lists and wikilinks. Raw HTML is
/// dropped at the parser and everything is sanitized with ammonia after.
///
/// Engine sugar:
/// - `__italic__` is `<em>`; underline is `++text++`.
/// - `||spoiler||`, `==mark==`, `==red|mark==` (fixed colors), `((kbd))`.
/// - `:shortcode:` is a Unicode emoji from a fixed table; any other valid name
///   is marked for the web layer's emotes (see [`EMOTE_OPEN`]).
/// - `>! Summary` with `> body` lines is a collapsible quote;
///   `:::details Title` and `:::pullquote` blocks end at `:::`.
/// - `:::infobox Title` is a side card: `Key = value` lines are its rows (a row
///   with no value is left out), anything else is Markdown in order.
/// - The first lone `[[toc]]` becomes a table of contents; footnotes collect
///   at the end in reference order.
/// - Fenced `mermaid`, `dot`, `graphviz`, `plantuml` and `math` keep their
///   text and gain a class for the worker.
/// - Only images under `/media/` render as images; others become links.
pub fn render_html(markdown: &str) -> String {
    let mut state = BlockState::for_document(markdown);
    render_html_with_depth(markdown, 0, &mut state)
}

/// Nesting depth is capped at 8 so a crafted chain of blocks cannot recurse
/// on input size.
fn render_html_with_depth(markdown: &str, depth: usize, state: &mut BlockState) -> String {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};

    if depth > 8 {
        return String::new();
    }

    let options = parser_options();

    let (without_blocks, blocks) = extract_custom_blocks(markdown, state);
    // pulldown-cmark hardwires `__` to `<strong>`.
    let mapped = map_double_underscore_to_italic(&without_blocks);

    // pulldown-cmark cannot refuse raw HTML, so its events are dropped here,
    // except a bare `<br>`. ammonia is the second line of defence.
    let parser = Parser::new_ext(&mapped, options).filter(|event| match event {
        Event::Html(html) | Event::InlineHtml(html) => is_allowed_raw_html(html),
        _ => true,
    });
    // An outside image would tell its host who read the article, and can change
    // after review. The stack pairs each image end with its start.
    // A local audio or video file plays in place, its alt text as the caption.
    #[derive(Clone, Copy)]
    enum Shown {
        Image,
        Link,
        Player,
    }
    let mut shown: Vec<Shown> = Vec::new();
    let parser = parser.map(move |event| match event {
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            if !is_local_image(&dest_url) {
                shown.push(Shown::Link);
                return Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                });
            }
            if let Some(player) = player_for(&dest_url) {
                shown.push(Shown::Player);
                let src = naw_core::html::escape(&dest_url);
                return Event::Html(
                    format!(
                        "<figure class=\"media-player\"><{player} controls preload=\"metadata\" src=\"{src}\"></{player}><figcaption>"
                    )
                    .into(),
                );
            }
            shown.push(Shown::Image);
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            })
        }
        Event::End(TagEnd::Image) => match shown.pop().unwrap_or(Shown::Image) {
            Shown::Link => Event::End(TagEnd::Link),
            Shown::Player => Event::Html("</figcaption></figure>".into()),
            Shown::Image => Event::End(TagEnd::Image),
        },
        other => other,
    });

    let mut dirty = String::with_capacity(mapped.len());
    pulldown_cmark::html::push_html(&mut dirty, parser);
    let dirty = restore_custom_blocks(&dirty, blocks, depth, state);
    let dirty = postprocess_diagrams(&dirty);
    // Runs on HTML, so inner formatting survives inside the wrappers.
    let mut dirty = postprocess_inline_spans(&dirty);
    // Alignment arrives as an inline style, which ammonia strips; `align` survives.
    dirty = preserve_table_alignment(&dirty);
    // ammonia drops the checkbox `<input>`s.
    dirty = render_task_items(&dirty);
    // Before heading ids, so `# 1` and `[^1]` never share an anchor.
    dirty = namespace_footnote_ids(&dirty);
    dirty = dirty.replace("<img src=", "<img loading=\"lazy\" decoding=\"async\" src=");
    let mut anchored = add_heading_ids(&dirty);
    anchored = dedupe_ids(&anchored);
    // The contents need final anchors.
    anchored = insert_toc(&anchored);
    anchored = link_footnote_references(&anchored);
    anchored = collect_footnotes(&anchored);
    // `id` and `class` keep anchors and author styling; `tabindex` keeps
    // spoilers keyboard operable; `open` keeps collapsible quotes working.
    ammonia::Builder::default()
        .add_generic_attributes(["id", "class", "tabindex"])
        .add_tag_attributes("details", ["open"])
        .add_tag_attributes("img", ["loading", "decoding"])
        .add_tags(["audio", "video"])
        .add_tag_attributes("audio", ["controls", "preload", "src"])
        .add_tag_attributes("video", ["controls", "preload", "src"])
        .clean(&anchored)
        .to_string()
}

/// The CommonMark extensions the engine enables.
fn parser_options() -> pulldown_cmark::Options {
    use pulldown_cmark::Options;
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_WIKILINKS
}

/// Outside images in `markdown`: the byte range of each destination URL in
/// the source, and the URL. Only inline `![alt](http...)` images whose
/// destination appears in the source as written; code, links and
/// reference-style images are left alone.
pub fn external_images(markdown: &str) -> Vec<(std::ops::Range<usize>, String)> {
    use pulldown_cmark::{Event, Parser, Tag};
    let mut found = Vec::new();
    for (event, range) in Parser::new_ext(markdown, parser_options()).into_offset_iter() {
        let Event::Start(Tag::Image { dest_url, .. }) = event else {
            continue;
        };
        if !(dest_url.starts_with("https://") || dest_url.starts_with("http://")) {
            continue;
        }
        let source = &markdown[range.clone()];
        let at = source
            .find("](")
            .and_then(|open| source[open..].find(dest_url.as_ref()).map(|pos| open + pos));
        if let Some(at) = at {
            let start = range.start + at;
            found.push((start..start + dest_url.len(), dest_url.to_string()));
        }
    }
    found
}

/// Images and links whose destination starts with one of `prefixes`, such as
/// `![Ferris](image:ferris.png)` or `[notes](file:notes.pdf)`: the byte range
/// of each destination in the source, the destination, and whether it is an
/// image. Code is left alone, like [`external_images`].
pub fn prefixed_destinations(
    markdown: &str,
    prefixes: &[&str],
) -> Vec<(std::ops::Range<usize>, String, bool)> {
    use pulldown_cmark::{Event, Parser, Tag};
    let mut found = Vec::new();
    for (event, range) in Parser::new_ext(markdown, parser_options()).into_offset_iter() {
        let (dest_url, is_image) = match event {
            Event::Start(Tag::Image { dest_url, .. }) => (dest_url, true),
            Event::Start(Tag::Link { dest_url, .. }) => (dest_url, false),
            _ => continue,
        };
        let lower = dest_url.to_ascii_lowercase();
        if !prefixes.iter().any(|p| lower.starts_with(p)) {
            continue;
        }
        let source = &markdown[range.clone()];
        let at = source
            .rfind("](")
            .and_then(|open| source[open..].find(dest_url.as_ref()).map(|pos| open + pos));
        if let Some(at) = at {
            let start = range.start + at;
            found.push((
                start..start + dest_url.len(),
                dest_url.to_string(),
                is_image,
            ));
        }
    }
    found
}

/// A file stored by this wiki: under `/media/`, with no way off the site.
fn is_local_image(url: &str) -> bool {
    url.starts_with("/media/") && !url.contains("//") && !url.contains('\\')
}

/// A local file that plays rather than shows: `audio` or `video`, by extension.
fn player_for(url: &str) -> Option<&'static str> {
    let ext = url.rsplit_once('.')?.1;
    match ext {
        "mp3" | "ogg" | "opus" | "flac" | "wav" | "m4a" => Some("audio"),
        "mp4" | "webm" => Some("video"),
        _ => None,
    }
}

/// The only raw HTML the parser lets through.
fn is_allowed_raw_html(html: &str) -> bool {
    matches!(
        html.trim().to_ascii_lowercase().as_str(),
        "<br>" | "<br/>" | "<br />"
    )
}

/// A `:::details`, `:::pullquote` or `>!` block, replaced by a placeholder
/// paragraph while Markdown runs.
struct CustomBlock {
    kind: BlockKind,
    title: String,
    body: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Details,
    Pullquote,
    Infobox,
    CollapsibleQuote,
}

/// Rows in one infobox; past it the lines stay Markdown.
const INFOBOX_ROWS_MAX: usize = 100;

/// Custom blocks per document, nesting included. Each renders through the
/// whole pipeline, so the count is bounded.
const BLOCK_MAX: usize = 256;

/// Shared by every nesting level of one render.
struct BlockState {
    /// `NAWBLOCK` plus a hash of the document, which the document cannot contain,
    /// so an author can never type a placeholder.
    tag: String,
    /// Blocks the document may still open; past it, fences stay text.
    left: usize,
}

impl BlockState {
    fn for_document(markdown: &str) -> Self {
        let hex: String = content_hash(markdown)[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Self {
            tag: format!("NAWBLOCK{hex}x"),
            left: BLOCK_MAX,
        }
    }

    fn placeholder(&self, idx: usize) -> String {
        format!("{}{idx}NAW", self.tag)
    }
}

/// Pulls custom blocks out of the Markdown so the parser never sees their
/// markers. Unclosed fences stay literal.
fn extract_custom_blocks(markdown: &str, state: &mut BlockState) -> (String, Vec<CustomBlock>) {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut blocks: Vec<CustomBlock> = Vec::new();
    let mut closers_left = true;
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let fence = if let Some(rest) = trimmed
            .strip_prefix(":::details")
            .filter(|_| trimmed == ":::details" || trimmed.starts_with(":::details "))
        {
            Some((BlockKind::Details, rest.trim().to_string()))
        } else if trimmed == ":::pullquote" || trimmed.starts_with(":::pullquote ") {
            Some((BlockKind::Pullquote, String::new()))
        } else {
            trimmed
                .strip_prefix(":::infobox")
                .filter(|_| trimmed == ":::infobox" || trimmed.starts_with(":::infobox "))
                .map(|rest| (BlockKind::Infobox, rest.trim().to_string()))
        };
        if let Some((kind, title)) = fence {
            let close = if closers_left {
                (i + 1..lines.len()).find(|&j| lines[j].trim() == ":::")
            } else {
                None
            };
            let Some(j) = close else {
                // No closer below means none below any later opener either; remembering
                // that keeps a page of unclosed fences linear.
                closers_left = false;
                out.push(lines[i].to_string());
                i += 1;
                continue;
            };
            if state.left == 0 {
                out.push(escape_fence(lines[i]));
                out.extend(lines[i + 1..j].iter().map(|line| line.to_string()));
                out.push(escape_fence(lines[j]));
                i = j + 1;
                continue;
            }
            let idx = blocks.len();
            blocks.push(CustomBlock {
                kind,
                title,
                body: lines[i + 1..j].join("\n"),
            });
            state.left -= 1;
            out.push(String::new());
            out.push(state.placeholder(idx));
            out.push(String::new());
            i = j + 1;
            continue;
        }
        if state.left > 0
            && let Some(summary) = parse_collapsible_opener(lines[i])
        {
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
            state.left -= 1;
            out.push(String::new());
            out.push(state.placeholder(idx));
            out.push(String::new());
            i = j;
            continue;
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    (out.join("\n"), blocks)
}

/// A fence kept as text with its colon escaped, so a bare `:::` does not
/// read as a definition list marker.
fn escape_fence(line: &str) -> String {
    let trimmed = line.trim_start();
    format!("{}\\{trimmed}", &line[..line.len() - trimmed.len()])
}

/// `>! Summary` with up to three leading spaces, as CommonMark quotes allow.
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

/// Strips one `>` prefix; `None` ends a collapsible group.
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

/// Swaps placeholders for rendered blocks in one pass. Titles stay escaped
/// text, and each block is used once.
fn restore_custom_blocks(
    html: &str,
    blocks: Vec<CustomBlock>,
    depth: usize,
    state: &mut BlockState,
) -> String {
    // Only the paragraph form, so a placeholder in a code span never matches.
    let open = format!("<p>{}", state.tag);
    let mut slots: Vec<Option<CustomBlock>> = blocks.into_iter().map(Some).collect();
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(pos) = rest.find(&open) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + open.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let block = after[digits..]
            .strip_prefix("NAW</p>")
            .and_then(|_| after[..digits].parse::<usize>().ok())
            .and_then(|idx| slots.get_mut(idx)?.take());
        match block {
            Some(block) => {
                out.push_str(&render_block(&block, depth, state));
                rest = &after[digits + "NAW</p>".len()..];
            }
            None => {
                out.push_str(&open);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn render_block(block: &CustomBlock, depth: usize, state: &mut BlockState) -> String {
    if block.kind == BlockKind::Infobox {
        return render_infobox(block, depth, state);
    }
    let body = if block.body.trim().is_empty() {
        String::new()
    } else {
        render_html_with_depth(&block.body, depth + 1, state)
    };
    let details = |class: &str, inner: String| {
        let summary = naw_core::html::escape(&block.title);
        if inner.is_empty() {
            format!("<details class=\"{class}\"><summary>{summary}</summary></details>")
        } else {
            format!("<details class=\"{class}\"><summary>{summary}</summary>\n{inner}\n</details>")
        }
    };
    match block.kind {
        BlockKind::Details => details("details", body),
        BlockKind::Pullquote => {
            format!("<figure class=\"pullquote\"><blockquote>\n{body}\n</blockquote></figure>")
        }
        BlockKind::CollapsibleQuote if body.is_empty() => details("quote", body),
        BlockKind::CollapsibleQuote => {
            details("quote", format!("<blockquote>\n{body}\n</blockquote>"))
        }
        BlockKind::Infobox => unreachable!("rendered by render_infobox"),
    }
}

/// `Key = value`: a short plain key, then the value. `None` for any other line.
fn infobox_row(line: &str) -> Option<(&str, &str)> {
    if line.starts_with([' ', '\t']) {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let plain = !key.is_empty()
        && key.chars().count() <= 60
        && !key.starts_with(['#', '!', '>', '-', '*', '|', '+'])
        && !key.contains(['[', ']', '(', ')', '`', '*', '_', '<']);
    plain.then(|| (key, value.trim()))
}

/// A side card: runs of rows become a definition list, and any other lines
/// render as Markdown where they stand.
fn render_infobox(block: &CustomBlock, depth: usize, state: &mut BlockState) -> String {
    enum Part<'a> {
        Rows(Vec<(&'a str, &'a str)>),
        Prose(Vec<&'a str>),
    }
    let mut parts: Vec<Part> = Vec::new();
    let mut row_count = 0;
    let mut in_fence = false;
    for line in block.body.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        let row = infobox_row(line).filter(|_| !in_fence && row_count < INFOBOX_ROWS_MAX);
        match (row, parts.last_mut()) {
            // An empty value leaves the row out, so optional fields vanish.
            (Some((_, "")), _) => {}
            (Some(row), Some(Part::Rows(rows))) => {
                rows.push(row);
                row_count += 1;
            }
            (Some(row), _) => {
                parts.push(Part::Rows(vec![row]));
                row_count += 1;
            }
            (None, Some(Part::Prose(lines))) => lines.push(line),
            (None, _) => parts.push(Part::Prose(vec![line])),
        }
    }

    let mut out = String::from("<aside class=\"infobox\">");
    if !block.title.trim().is_empty() {
        out.push_str("<p class=\"infobox-title\">");
        out.push_str(&naw_core::html::escape(block.title.trim()));
        out.push_str("</p>");
    }
    for part in parts {
        match part {
            Part::Prose(lines) => {
                let text = lines.join("\n");
                if !text.trim().is_empty() {
                    out.push_str(&render_html_with_depth(&text, depth + 1, state));
                }
            }
            Part::Rows(rows) => {
                out.push_str("<dl class=\"infobox-rows\">");
                for (key, value) in rows {
                    let html = render_html_with_depth(value, depth + 1, state);
                    let html = html.trim();
                    // One paragraph is the usual value; its wrapper would add a margin.
                    let inner = html
                        .strip_prefix("<p>")
                        .and_then(|rest| rest.strip_suffix("</p>"))
                        .filter(|inner| !inner.contains("<p>"))
                        .unwrap_or(html);
                    out.push_str("<div><dt>");
                    out.push_str(&naw_core::html::escape(key));
                    out.push_str("</dt><dd>");
                    out.push_str(inner);
                    out.push_str("</dd></div>");
                }
                out.push_str("</dl>");
            }
        }
    }
    out.push_str("</aside>");
    out
}

/// Rewrites `__italic__` to `*italic*` outside code, link destinations and
/// math. `___triple___` is left to the parser.
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

/// Diagram and math fences keep their text and gain a class for the worker.
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

/// Inline sugar on rendered HTML: spoilers, marks, underline, kbd and
/// shortcodes. Code, `<pre>` and math are skipped.
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

/// Highlight colors for `==color|text==`, a closed set so author input never
/// reaches a class name.
const MARK_COLORS: &[&str] = &[
    "red", "orange", "yellow", "green", "blue", "violet", "pink", "gray",
];

/// `==red|text==` to `<mark class="mark-red">`; unknown colors fall through
/// to plain `==mark==`.
fn replace_mark_color(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let d = ['=', '='];
    let delims = Counts::new(chars.len(), |i| matches_delim(&chars, i, &d));
    let solid = Counts::new(chars.len(), |i| !chars[i].is_whitespace());
    let mut closers = Closers::default();
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
                && let Some(end) = closers.find(j + 1, |from| find_valid_closer(&chars, from, &d))
                && solid.any(j + 1, end)
                && !delims.any(j + 1, end + 1 - d.len())
            {
                out.push_str(&format!("<mark class=\"mark-{name}\">"));
                out.extend(&chars[j + 1..end]);
                out.push_str("</mark>");
                i = end + 2;
                continue;
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

/// Splits HTML into protected (`<pre>`, `<code>`, math) and normal segments.
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
            // Step by character: `&html[i..]` must stay on a char boundary.
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

/// Replaces `||a||`-style pairs with `open_tag`/`close_tag`. Flanking rules
/// keep `C++`, `x==y` and `a||b` literal; the first valid closer wins and
/// empty pairs stay.
fn replace_delimited(text: &str, delim: &str, open_tag: &str, close_tag: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let d: Vec<char> = delim.chars().collect();
    // Span checks by running counts, so rejected openers cost no walk.
    let delims = Counts::new(chars.len(), |i| matches_delim(&chars, i, &d));
    let solid = Counts::new(chars.len(), |i| !chars[i].is_whitespace());
    let mut closers = Closers::default();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if matches_delim(&chars, i, &d) && is_valid_opener(&chars, i, d.len()) {
            let from = i + d.len();
            if let Some(end) = closers.find(from, |from| find_valid_closer(&chars, from, &d))
                && solid.any(from, end)
                && !delims.any(from, end + 1 - d.len())
            {
                out.push_str(open_tag);
                out.extend(&chars[from..end]);
                out.push_str(close_tag);
                i = end + d.len();
                continue;
            }
            for _ in 0..d.len() {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        // Skip tags so `class="a==b"` never matches.
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

/// The first valid closer on the line, and where the walk stopped.
fn find_valid_closer(chars: &[char], from: usize, d: &[char]) -> (Option<usize>, usize) {
    let mut i = from;
    while i + d.len() <= chars.len() {
        if chars[i] == '\n' {
            return (None, i);
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
                return (Some(i), i);
            }
        }
        i += 1;
    }
    (None, i)
}

/// `((Ctrl+C))` to `<kbd>Ctrl+C</kbd>`, with parentheses as the flanking
/// guard so smileys like `:((` stay literal.
fn replace_kbd(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let pairs = Counts::new(chars.len(), |i| {
        i + 1 < chars.len() && chars[i] == chars[i + 1] && matches!(chars[i], '(' | ')')
    });
    let solid = Counts::new(chars.len(), |i| !chars[i].is_whitespace());
    let mut closers = Closers::default();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let opens = i + 1 < chars.len() && chars[i] == '(' && chars[i + 1] == '(';
        let prev_ok = i == 0 || chars[i - 1] != '(';
        let next_ok = i + 2 < chars.len() && !chars[i + 2].is_whitespace();
        if opens && prev_ok && next_ok {
            let from = i + 2;
            if let Some(end) = closers.find(from, |from| find_kbd_close(&chars, from))
                && solid.any(from, end)
                && !pairs.any(from, end - 1)
            {
                out.push_str("<kbd>");
                out.extend(&chars[from..end]);
                out.push_str("</kbd>");
                i = end + 2;
                continue;
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

/// As [`find_valid_closer`], for `))`.
fn find_kbd_close(chars: &[char], from: usize) -> (Option<usize>, usize) {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '\n' {
            return (None, i);
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
                return (Some(i), i);
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    (None, i)
}

/// `:fire:` shortcodes to Unicode emoji. Times like `12:30` and URLs stay
/// literal by the flanking rules.
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
            // Maybe one of the wiki's emotes; the web layer decides when it serves.
            if closed && next_ok && is_emote_name(&name) {
                out.push_str(EMOTE_OPEN);
                out.push(':');
                out.push_str(&name);
                out.push(':');
                out.push_str(EMOTE_CLOSE);
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

/// Marks an emote reference, `:name:` inside this span. Only the renderer
/// writes it, so the web layer can trust its contents.
pub const EMOTE_OPEN: &str = "<span class=\"naw-emote\">";
pub const EMOTE_CLOSE: &str = "</span>";

/// Whether `name` can be written as `:name:`: an ASCII letter or digit, then
/// letters, digits, `_`, `-` and `+`, at most 64.
pub fn is_emote_name(name: &str) -> bool {
    name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(is_shortcode_char)
}

/// Whether `:name:` is a Unicode emoji, which always wins over an emote.
pub fn is_unicode_shortcode(name: &str) -> bool {
    emoji_for(name).is_some()
}

fn is_shortcode_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+'
}

/// Fixed shortcode table; plain Unicode, so no assets are needed.
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

/// Rewrites the writer's inline alignment style to the `align` attribute.
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

/// Moves footnote anchors into the `fn-` namespace on both ends.
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

/// Makes every id unique in document order: the first keeps it, later ones
/// gain `-2`, `-3`, and renamed footnotes pull their references along.
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
    if renames.is_empty() {
        return out;
    }
    let mut targets: HashMap<String, String> = HashMap::new();
    for (old, new) in renames {
        targets.entry(old).or_insert(new);
    }
    const REF: &str = "<sup class=\"footnote-reference\"><a href=\"#";
    let mut linked = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(pos) = rest.find(REF) {
        let split = pos + REF.len();
        linked.push_str(&rest[..split]);
        rest = &rest[split..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let target = &rest[..end];
        match targets.get(target) {
            Some(new) if rest[end..].starts_with("\">") => linked.push_str(new),
            _ => linked.push_str(target),
        }
        rest = &rest[end..];
    }
    linked.push_str(rest);
    linked
}
/// Replaces the first lone `[[toc]]` with a nav of the headings, on final
/// HTML so links match the assigned ids; later markers vanish.
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
        // Already escaped by the renderer.
        let text = strip_inline_tags(inner).trim().to_string();
        if !text.is_empty() {
            items.push((n, id, text));
        }
        rest = &rest[content_start + content_end + close.len()..];
    }
    // A single `#` heading is the page title, not a section.
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
    // Every nav copy is as large as all headings, so only the first is kept.
    const TOC: &str = "<p>[[toc]]</p>";
    let html = html.replace("<p><a href=\"toc\">toc</a></p>", TOC);
    match html.split_once(TOC) {
        Some((head, tail)) => format!("{head}{nav}{}", tail.replace(TOC, "")),
        None => html,
    }
}

/// `attr="value"` from a tag fragment; the first match wins.
fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let key = format!("{attr}=\"");
    let pos = tag.find(&key)?;
    let start = pos + key.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

/// Moves footnote definitions into one block at the end, in reference order.
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
    let anchored = anchored_references(html);
    out.push_str("<div class=\"footnotes\">");
    for def in defs {
        let back = attr_value(def, "id")
            .and_then(|id| id.strip_prefix("fn-").map(|n| format!("fnref-{n}")))
            .filter(|back| anchored.contains(back.as_str()));
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

/// Anchors of footnote references, collected in one pass.
fn anchored_references(html: &str) -> HashSet<&str> {
    const OPEN: &str = "<sup class=\"footnote-reference\" id=\"";
    let mut found = HashSet::new();
    let mut rest = html;
    while let Some(pos) = rest.find(OPEN) {
        let after = &rest[pos + OPEN.len()..];
        let Some(end) = after.find('"') else {
            break;
        };
        if after[end..].starts_with("\">") {
            found.insert(&after[..end]);
        }
        rest = &after[end..];
    }
    found
}

/// Every `id` value in the document, collected in one pass.
fn ids_in(html: &str) -> HashSet<&str> {
    let mut ids = HashSet::new();
    let mut rest = html;
    while let Some(pos) = rest.find("id=\"") {
        let after = &rest[pos + 4..];
        let Some(end) = after.find('"') else {
            break;
        };
        ids.insert(&after[..end]);
        rest = &after[end..];
    }
    ids
}

/// Anchors the first reference to each footnote (`fnref-1` for `fn-1`) so
/// the note can link back; an id already in use wins.
fn link_footnote_references(html: &str) -> String {
    const OPEN: &str = "<sup class=\"footnote-reference\"><a href=\"#fn-";
    let mut out = String::with_capacity(html.len() + 64);
    let taken_ids = ids_in(html);
    let mut linked = HashSet::new();
    let mut rest = html;
    while let Some(pos) = rest.find(OPEN) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + OPEN.len()..];
        let Some(quote) = after.find('"') else {
            break;
        };
        let label = &after[..quote];
        let anchor = format!("fnref-{label}");
        let taken = taken_ids.contains(anchor.as_str());
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

/// Turns disabled checkboxes into marks the sanitizer keeps; the state stays
/// in the text as `[x]` or `[ ]`.
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
/// Gives every heading a stable `id`; duplicates get `-2`, `-3`.
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

/// Splits a trailing `{id="..." class="..."}` off heading HTML. Other keys
/// are dropped; a malformed block stays literal.
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

/// Render pipeline version, part of the `render_cache` key. Bump it whenever
/// the output changes for the same input.
pub const RENDERER_VERSION: i32 = 17;

/// A rendered body fragment and its cache key.
pub struct RenderedBody {
    pub content_hash: Vec<u8>,
    pub html: String,
    /// Time spent in the Markdown stage.
    pub render_ms: u64,
}

/// The body hash: `revisions.content_hash` and the `render_cache` key.
pub fn content_hash(body_md: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(body_md.as_bytes()).to_vec()
}

/// Renders a body fragment, timed, with its cache key. The chrome around it
/// is per visitor and stays out of the cache.
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
    fn only_local_images_render_as_images() {
        let local = render_html("![Filian](/media/ab/abc.png)");
        assert!(local.contains("<img"), "{local}");
        assert!(local.contains(r#"loading="lazy""#), "{local}");
        assert!(local.contains(r#"src="/media/ab/abc.png""#), "{local}");
        let outside = render_html("![tracker](https://example.com/pixel.png)");
        assert!(!outside.contains("<img"), "{outside}");
        assert!(
            outside.contains(r#"href="https://example.com/pixel.png""#),
            "{outside}"
        );
        assert!(outside.contains(">tracker</a>"), "{outside}");
        for sneaky in [
            "//evil.example/x.png",
            "/media//evil.example/x.png",
            "/mediax/y.png",
        ] {
            let html = render_html(&format!("![x]({sneaky})"));
            assert!(!html.contains("<img"), "{sneaky}: {html}");
        }
    }

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
        // ammonia allows <div>, so only the parser filter removes this.
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
    fn local_audio_and_video_play_with_their_alt_as_caption() {
        let html = render_html("![Theme song](/media/ab/c.ogg)\n\n![Clip](/media/ab/d.webm)\n");
        assert!(html.contains("<audio controls=\"\" preload=\"metadata\" src=\"/media/ab/c.ogg\"></audio><figcaption>Theme song</figcaption>"), "{html}");
        assert!(html.contains("<video controls"), "{html}");
        // An outside file never plays; it stays a link.
        let outside = render_html("![x](https://evil.test/a.mp3)\n");
        assert!(!outside.contains("<audio"), "{outside}");
    }

    #[test]
    fn prefixed_destinations_are_found_in_images_and_links_but_not_code() {
        let md = "![F](image:ferris.png) and [notes](file:notes.pdf) `![x](image:no.png)`";
        let found = prefixed_destinations(md, &["image:", "file:"]);
        assert_eq!(found.len(), 2);
        assert_eq!(&md[found[0].0.clone()], "image:ferris.png");
        assert!(found[0].2);
        assert_eq!(&md[found[1].0.clone()], "file:notes.pdf");
        assert!(!found[1].2);
    }

    #[test]
    fn an_infobox_has_a_title_rows_and_prose_in_order() {
        let html = render_html(
            ":::infobox Filian\n![Filian](/media/ab/c.png)\nDebut = 2021\nSite = [link](https://example.com)\nAgency =\n\nA **fox** VTuber.\n:::\n",
        );
        assert!(html.starts_with("<aside class=\"infobox\"><p class=\"infobox-title\">Filian</p>"));
        assert!(html.contains("<img"));
        assert!(html.contains("<div><dt>Debut</dt><dd>2021</dd></div>"));
        assert!(html.contains("<dt>Site</dt><dd><a href=\"https://example.com\""));
        // An empty value leaves its row out.
        assert!(!html.contains("Agency"));
        assert!(html.contains("<strong>fox</strong>"));
        assert!(html.find("Debut") < html.find("fox"));
    }

    #[test]
    fn infobox_titles_and_keys_are_text() {
        let html = render_html(":::infobox <script>x</script>\nKey<b> = v\n:::\n");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<b>"));
        // `<` in a key makes it prose, which the parser strips of HTML.
        assert!(!html.contains("<dt>Key"));
    }

    #[test]
    fn render_body_returns_the_fragment_and_its_key() {
        let rendered = render_body("# Hi");
        assert_eq!(rendered.html, render_html("# Hi"));
        assert!(rendered.html.contains("<h1 id=\"hi\">Hi</h1>"));
        assert!(!rendered.html.contains("<html"));
        assert!(!rendered.html.contains("<title"));
        assert_eq!(rendered.content_hash, content_hash("# Hi"));
    }

    #[test]
    fn the_cache_key_is_the_revision_hash_and_nothing_else() {
        // Only the body may change the key.
        let a = content_hash("# Hi");
        assert_eq!(a, content_hash("# Hi"));
        assert_eq!(a, render_body("# Hi").content_hash);
        assert_ne!(a, content_hash("# Bye"));
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
    fn outside_images_are_found_by_their_exact_source_range() {
        let md = "![a](https://x.test/a.png) and [link](https://x.test/l)\n\n`![c](https://x.test/c.png)`\n\n![local](/media/ab/x.png) ![b](http://y.test/b.jpg \"t\")\n";
        let found = external_images(md);
        let urls: Vec<&str> = found.iter().map(|(_, u)| u.as_str()).collect();
        assert_eq!(urls, vec!["https://x.test/a.png", "http://y.test/b.jpg"]);
        for (range, url) in &found {
            assert_eq!(&md[range.clone()], url);
        }
    }

    #[test]
    fn unknown_shortcodes_are_marked_for_emotes() {
        let html = render_html("Filian :catJAM: and :KEKW: but `:code:` stays.\n");
        assert!(
            html.contains(r#"<span class="naw-emote">:catJAM:</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="naw-emote">:KEKW:</span>"#),
            "{html}"
        );
        assert!(html.contains("<code>:code:</code>"), "{html}");
        assert!(is_emote_name("peepoHappy") && is_emote_name("a-b_c+1"));
        assert!(
            !is_emote_name("")
                && !is_emote_name("_x")
                && !is_emote_name("a b")
                && !is_emote_name("Кот")
        );
        assert!(is_unicode_shortcode("fire") && !is_unicode_shortcode("catJAM"));
    }

    #[test]
    fn details_summary_survives_sanitize() {
        let html = render_html(">! Title\n> body\n");
        assert!(html.contains("<details"), "{html}");
        assert!(html.contains("<summary>"), "{html}");
    }

    #[test]
    fn a_typed_placeholder_is_only_text() {
        // The old fixed placeholder, typed by an author.
        let md = format!(
            ":::details T\nbody\n:::\n\n{}",
            "NAWBLOCK0NAW\n\n".repeat(50)
        );
        let html = render_html(&md);
        assert_eq!(html.matches("<details").count(), 1, "{html}");
        assert_eq!(html.matches("NAWBLOCK0NAW").count(), 50, "{html}");
    }

    #[test]
    fn nested_placeholders_do_not_multiply() {
        // Eight levels of `>!` with typed placeholders: any swap would be exponential.
        let mut md = String::from("core\n");
        for level in 0..8 {
            let quoted: String = md.lines().map(|line| format!("> {line}\n")).collect();
            md = format!(
                ">! level {level}\n{quoted}>\n{}",
                "> NAWBLOCK0NAW\n>\n".repeat(20)
            );
        }
        let html = render_html(&md);
        assert!(html.len() < md.len() * 8, "{} bytes", html.len());
    }

    #[test]
    fn blocks_past_the_budget_stay_literal() {
        let md = ":::details T\nbody\n:::\n\n".repeat(BLOCK_MAX + 10);
        let html = render_html(&md);
        assert_eq!(html.matches("<details").count(), BLOCK_MAX);
        assert_eq!(html.matches("<p>:::details T\nbody\n:::</p>").count(), 10);
        assert!(!html.contains("NAWBLOCK"), "{html}");
        assert!(!html.contains("<dt>"), "{html}");
    }

    #[test]
    fn only_the_first_toc_marker_gets_the_nav() {
        let md = format!("{}# A\n\n## B\n\n## C\n", "[[toc]]\n\n".repeat(30));
        let html = render_html(&md);
        assert_eq!(html.matches("<nav class=\"toc\">").count(), 1, "{html}");
        assert!(!html.contains("[[toc]]"), "{html}");
    }

    #[test]
    fn failed_openers_do_not_hide_a_later_pair() {
        let html = render_html("||a ||b||");
        assert!(
            html.contains("||a <span class=\"spoiler\" tabindex=\"0\">b</span>"),
            "{html}"
        );
        let html = render_html("((a ((b))");
        assert!(html.contains("((a <kbd>b</kbd>"), "{html}");
        let html = render_html("==red|a ==red|b==");
        assert!(
            html.contains("==red|a <mark class=\"mark-red\">b</mark>"),
            "{html}"
        );
    }

    #[test]
    fn a_line_of_unclosed_openers_stays_fast() {
        // Loose bound: it separates linear from quadratic.
        let line = " ||a ==a ++a ((a ==red|a".repeat(10_000);
        let started = std::time::Instant::now();
        let html = render_html(&line);
        assert!(!html.contains("<span class=\"spoiler\""));
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
    }

    #[test]
    fn many_footnotes_render_with_their_way_back() {
        let refs: String = (0..3_000).map(|n| format!("x[^{n}] ")).collect();
        let defs: String = (0..3_000)
            .map(|n| format!("[^{n}]: note {n}\n\n"))
            .collect();
        let html = render_html(&format!("{refs}\n\n{defs}"));
        assert_eq!(html.matches("footnote-backref").count(), 3_000);
    }
}

#[cfg(test)]
mod non_ascii {
    //! Every position walk must step by whole characters, or a two-byte
    //! Cyrillic letter panics the renderer.
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
