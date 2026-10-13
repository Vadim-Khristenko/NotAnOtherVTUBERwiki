//! Wikitext to Markdown.
//!
//! The text goes through four passes. The first takes out what must not be
//! touched (`<nowiki>`, `<pre>`, code, math) and what moves (`<ref>` becomes
//! a footnote, `<tabber>` details blocks, `<gallery>` pictures). The second
//! walks template calls: it keeps them, records their names, and resolves
//! the magic words and parser functions the engine does not have. The third
//! works line by line: headings, lists, tables, indented code and block
//! HTML. The last works inside a line: links, files, bold and italic, tags.
//!
//! What moves out of the way waits in a stash, behind a private-use marker,
//! and comes back at the end, so no later pass can mangle it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::slug::{file_name, page_slug};

/// Opens a stashed piece that sits inside a line.
const OPEN: char = '\u{E000}';
/// Opens a stashed piece that is a block of its own lines.
const BLOCK_OPEN: char = '\u{E002}';
/// Closes either.
const CLOSE: char = '\u{E001}';

/// What a page needs from the wiki around it.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The page's own title, for `{{PAGENAME}}`.
    pub title: String,
    /// Local names of the category namespace, besides `Category`.
    pub category_names: Vec<String>,
    /// Local names of the file namespace, besides `File` and `Image`.
    pub file_names: Vec<String>,
    /// The page is a template, so `{{PAGENAME}}` means the page calling it.
    pub template: bool,
    /// Template parameters that take a picture, by the template's slug
    /// (see [`crate::infobox::image_params`]): their file names become the
    /// engine's file addresses.
    pub image_params: BTreeMap<String, BTreeSet<String>>,
}

/// What a conversion carried over and what it could not.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Constructs converted, by kind: `heading`, `link`, `table`...
    pub converted: BTreeMap<String, usize>,
    /// Constructs dropped or left as text, by kind, with an example.
    pub unsupported: BTreeMap<String, Unsupported>,
    /// Templates called, by the slug of their page, and how often.
    pub templates: BTreeMap<String, usize>,
    /// Files shown or linked, by their name on the source wiki.
    pub files: BTreeSet<String>,
    /// Pages linked, by their address here.
    pub links: BTreeSet<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Unsupported {
    pub count: usize,
    /// The first piece of source that hit it, shortened.
    pub example: String,
}

impl Report {
    fn ok(&mut self, kind: &str) {
        *self.converted.entry(kind.to_string()).or_default() += 1;
    }

    fn miss(&mut self, kind: &str, example: &str) {
        let entry = self.unsupported.entry(kind.to_string()).or_default();
        if entry.count == 0 {
            entry.example = example.chars().take(80).collect();
        }
        entry.count += 1;
    }

    /// Adds another page's report to this one.
    pub fn merge(&mut self, other: &Report) {
        for (k, v) in &other.converted {
            *self.converted.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &other.unsupported {
            let entry = self.unsupported.entry(k.clone()).or_default();
            if entry.count == 0 {
                entry.example = v.example.clone();
            }
            entry.count += v.count;
        }
        for (k, v) in &other.templates {
            *self.templates.entry(k.clone()).or_default() += v;
        }
        self.files.extend(other.files.iter().cloned());
        self.links.extend(other.links.iter().cloned());
    }
}

/// One page converted.
#[derive(Debug, Clone)]
pub struct Converted {
    pub markdown: String,
    /// The title a redirect leads to; the page then has no text of its own.
    pub redirect: Option<String>,
    pub report: Report,
}

/// Converts one page's wikitext.
pub fn convert(wikitext: &str, options: &Options) -> Converted {
    let mut cx = Cx {
        options,
        report: Report::default(),
        stash: Vec::new(),
        notes: Vec::new(),
        note_names: HashMap::new(),
    };
    let text = wikitext.replace("\r\n", "\n");
    let text = strip_comments(&text);
    if let Some(target) = redirect_target(&text) {
        cx.report.ok("redirect");
        return Converted {
            markdown: String::new(),
            redirect: Some(target),
            report: cx.report,
        };
    }
    let text = cx.portable_infoboxes(&text);
    let text = cx.protect(&text);
    let text = cx.refs(&text);
    let text = cx.block_tags(&text);
    let text = cx.switches(&text);
    let text = cx.templates(&text);
    let mut out = cx.blocks(&text);
    out = out.trim().to_string();
    if !cx.notes.is_empty() {
        out.push_str("\n\n");
        let notes = std::mem::take(&mut cx.notes);
        for (label, raw) in notes {
            if raw.trim().is_empty() {
                cx.report.miss("footnote without text", &label);
            }
            let raw = cx.templates(&raw.replace('\n', " "));
            let content = cx.inline(raw.trim(), true);
            out.push_str(&format!("[^{label}]: {content}\n"));
        }
    }
    let out = cx.restore(&out);
    Converted {
        markdown: tidy(&out),
        redirect: None,
        report: cx.report,
    }
}

struct Cx<'a> {
    options: &'a Options,
    report: Report,
    stash: Vec<String>,
    /// Footnotes in order: label and wikitext.
    notes: Vec<(String, String)>,
    /// A named `<ref>`'s label.
    note_names: HashMap<String, String>,
}

impl Cx<'_> {
    fn put(&mut self, piece: String) -> String {
        self.stash.push(piece);
        format!("{OPEN}{}{CLOSE}", self.stash.len() - 1)
    }

    /// A stashed block, on lines of its own.
    fn put_block(&mut self, piece: String) -> String {
        self.stash.push(piece);
        format!("\n\n{BLOCK_OPEN}{}{CLOSE}\n\n", self.stash.len() - 1)
    }

    fn restore(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find([OPEN, BLOCK_OPEN]) {
            out.push_str(&rest[..at]);
            let after = &rest[at + OPEN.len_utf8()..];
            let Some(end) = after.find(CLOSE) else {
                rest = after;
                continue;
            };
            // A stashed piece may hold another, as code inside a table.
            if let Some(piece) = after[..end]
                .parse::<usize>()
                .ok()
                .and_then(|n| self.stash.get(n))
            {
                out.push_str(&self.restore(piece));
            }
            rest = &after[end + CLOSE.len_utf8()..];
        }
        out.push_str(rest);
        out
    }

    // -----------------------------------------------------------------
    // Pass 1: what must not be touched, and what moves
    // -----------------------------------------------------------------

    /// FANDOM's `<infobox>` as a `:::infobox` block.
    fn portable_infoboxes(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some((start, end, infobox)) = crate::infobox::find(rest) {
            out.push_str(&rest[..start]);
            self.report.ok("portable infobox");
            let opener = self.put_block(format!(":::infobox {}", infobox.title));
            out.push_str(&opener);
            out.push_str(&infobox.lines.join("\n"));
            let closer = self.put_block(":::".to_string());
            out.push_str(&closer);
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }

    /// `<nowiki>`, `<pre>`, `<syntaxhighlight>`, `<source>`, `<code>`, `<math>`.
    fn protect(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(tag) = next_tag(
            rest,
            &["nowiki", "pre", "syntaxhighlight", "source", "code", "math"],
        ) {
            out.push_str(&rest[..tag.start]);
            let content = &rest[tag.content.clone()];
            let piece = match tag.name.as_str() {
                "nowiki" => {
                    self.report.ok("nowiki");
                    self.put(escape_all(content))
                }
                "pre" => {
                    self.report.ok("code block");
                    self.put_block(fence("", content))
                }
                "syntaxhighlight" | "source" => {
                    self.report.ok("code block");
                    let lang = attr_value(&tag.attrs, "lang").unwrap_or_default();
                    if has_word(&tag.attrs, "inline") || !content.contains('\n') && tag.inline {
                        self.put(code_span(content))
                    } else {
                        self.put_block(fence(&lang, content))
                    }
                }
                "code" => {
                    self.report.ok("code");
                    if content.contains(['<', '[', '{', '\'']) {
                        // Markup inside <code> still renders in MediaWiki;
                        // keep the tag and let the later passes see it.
                        format!("<code>{content}</code>")
                    } else {
                        self.put(code_span(content))
                    }
                }
                _ => {
                    self.report.ok("math");
                    let display = attr_value(&tag.attrs, "display").as_deref() == Some("block");
                    if display {
                        self.put_block(fence("math", content.trim()))
                    } else {
                        self.put(format!("${}$", content.trim()))
                    }
                }
            };
            out.push_str(&piece);
            rest = &rest[tag.end..];
        }
        out.push_str(rest);
        out
    }

    /// `<ref>` becomes `[^n]`; the text goes to the notes at the end.
    /// `<references>` goes away, keeping the notes defined inside it.
    fn refs(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(tag) = next_tag(rest, &["references", "ref"]) {
            out.push_str(&rest[..tag.start]);
            if tag.name == "references" {
                // Notes defined in the list: register them, show nothing.
                let inner = rest[tag.content.clone()].to_string();
                let mut inner_rest = inner.as_str();
                while let Some(r) = next_tag(inner_rest, &["ref"]) {
                    self.note(&r.attrs, Some(&inner_rest[r.content.clone()]));
                    inner_rest = &inner_rest[r.end..];
                }
                self.report.ok("reference list");
            } else {
                let content = (!tag.empty).then(|| &rest[tag.content.clone()]);
                let label = self.note(&tag.attrs, content);
                let mark = self.put(format!("[^{label}]"));
                out.push_str(&mark);
                self.report.ok("footnote");
            }
            rest = &rest[tag.end..];
        }
        out.push_str(rest);
        out
    }

    /// Registers a note and returns its label. A named note keeps its first
    /// label and takes the first text it is given.
    fn note(&mut self, attrs: &str, content: Option<&str>) -> String {
        let name = attr_value(attrs, "name").filter(|n| !n.is_empty());
        if attr_value(attrs, "group").is_some() {
            // Notes and sources share one list here.
            self.report.ok("footnote group merged");
        }
        if let Some(name) = &name
            && let Some(label) = self.note_names.get(name).cloned()
        {
            if let Some(content) = content
                && let Some(slot) = self.notes.iter_mut().find(|(l, _)| *l == label)
                && slot.1.trim().is_empty()
            {
                slot.1 = content.to_string();
            }
            return label;
        }
        let label = (self.notes.len() + 1).to_string();
        self.notes
            .push((label.clone(), content.unwrap_or_default().to_string()));
        if let Some(name) = name {
            self.note_names.insert(name, label.clone());
        }
        label
    }

    /// `<tabber>`, `<gallery>` and `<poem>`.
    fn block_tags(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(tag) = next_tag(rest, &["tabber", "gallery", "poem"]) {
            out.push_str(&rest[..tag.start]);
            let content = rest[tag.content.clone()].to_string();
            match tag.name.as_str() {
                "tabber" => {
                    self.report.ok("tabs");
                    let opener = self.put_block(":::tabs".to_string());
                    out.push_str(&opener);
                    for (n, tab) in content.split("|-|").enumerate() {
                        let (name, body) = tab.split_once('=').unwrap_or(("", tab));
                        let name = plain(name.trim());
                        let name = if name.is_empty() {
                            (n + 1).to_string()
                        } else {
                            name
                        };
                        let title = self.put_block(format!("## {name}"));
                        out.push_str(&title);
                        out.push_str(body.trim());
                    }
                    let closer = self.put_block(":::".to_string());
                    out.push_str(&closer);
                }
                "gallery" => {
                    self.report.ok("gallery");
                    out.push_str("\n\n");
                    for line in content.lines().map(str::trim).filter(|l| !l.is_empty()) {
                        let mut parts = split_top(line, "|");
                        let file = parts.remove(0);
                        let file =
                            strip_file_prefix(file.trim(), self.options).unwrap_or(file.trim());
                        let caption = parts.last().map(|c| plain(c)).unwrap_or_default();
                        match self.file_markdown(file, &caption, true) {
                            Some(md) => {
                                out.push_str(&md);
                                out.push('\n');
                            }
                            None => self.report.miss("file name", file),
                        }
                    }
                    out.push('\n');
                }
                _ => {
                    self.report.ok("poem");
                    let lines: Vec<&str> = content.trim_matches('\n').lines().collect();
                    out.push_str(&lines.join("<br>\n"));
                }
            }
            rest = &rest[tag.end..];
        }
        out.push_str(rest);
        out
    }

    /// `__TOC__` and the other behaviour switches.
    fn switches(&mut self, text: &str) -> String {
        if !text.contains("__") {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find("__") {
            let after = &rest[at + 2..];
            let word_len = after.chars().take_while(|c| c.is_ascii_uppercase()).count();
            if word_len >= 3 && after[word_len..].starts_with("__") {
                out.push_str(&rest[..at]);
                let word = &after[..word_len];
                match word {
                    "TOC" | "FORCETOC" => {
                        self.report.ok("table of contents");
                        let toc = self.put_block("[[toc]]".to_string());
                        out.push_str(&toc);
                    }
                    _ => self.report.ok("behaviour switch"),
                }
                rest = &after[word_len + 2..];
            } else {
                out.push_str(&rest[..at + 2]);
                rest = after;
            }
        }
        out.push_str(rest);
        out
    }

    // -----------------------------------------------------------------
    // Pass 2: templates, magic words and parser functions
    // -----------------------------------------------------------------

    fn templates(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        let bytes = text.as_bytes();
        let mut braces = Braces::new(text);
        while i < bytes.len() {
            if bytes[i..].starts_with(b"{{{")
                && let Some(end) = braces.triple(i)
            {
                out.push_str(&text[i..end]);
                i = end;
                continue;
            }
            if bytes[i..].starts_with(b"{{") {
                if let Some(end) = braces.double(i) {
                    let inner = &text[i + 2..end - 2];
                    out.push_str(&self.template_call(inner));
                    i = end;
                    continue;
                }
                self.report.miss("unclosed template", &text[i..]);
            }
            let c = text[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
        out
    }

    fn template_call(&mut self, inner: &str) -> String {
        let trimmed = inner.trim();
        let name_end = trimmed.find(['|', ':']).unwrap_or(trimmed.len());
        let head = trimmed[..name_end].trim();
        let colon = trimmed[name_end..].starts_with(':');
        let after_colon = if colon { &trimmed[name_end + 1..] } else { "" };

        // Parser functions.
        if let Some(function) = head.strip_prefix('#') {
            let function = function.trim().to_ascii_lowercase();
            return match function.as_str() {
                "if" | "ifeq" | "switch" => {
                    self.report.ok("parser function");
                    format!("{{{{{}}}}}", self.templates(inner))
                }
                "ev" | "evt" | "evu" | "evl" => self.embed_video(after_colon, inner),
                "tag" => self.tag_function(after_colon, inner),
                "invoke" => {
                    self.report.miss("Lua module call", inner);
                    String::new()
                }
                _ => {
                    self.report.miss(&format!("{{{{#{function}}}}}"), inner);
                    String::new()
                }
            };
        }

        // Magic words and functions spelled with a colon.
        let upper = head.to_ascii_uppercase();
        match upper.as_str() {
            "PAGENAME" | "FULLPAGENAME" | "BASEPAGENAME" | "SUBPAGENAME" | "ROOTPAGENAME"
            | "PAGENAMEE" | "ARTICLEPAGENAME" => {
                if self.options.template {
                    self.report.miss("{{PAGENAME}} in a template", inner);
                    return String::new();
                }
                self.report.ok("magic word");
                return self.options.title.clone();
            }
            "PAGELANGUAGE" | "!" => return format!("{{{{{head}}}}}"),
            "=" => return "=".to_string(),
            "DISPLAYTITLE" | "DEFAULTSORT" | "DEFAULTSORTKEY" | "DEFAULTCATEGORYSORT" => {
                self.report.ok("behaviour switch");
                return String::new();
            }
            "LC" | "UC" | "LCFIRST" | "UCFIRST" if colon => {
                self.report.ok("magic word");
                let arg = self.templates(after_colon.trim());
                return match upper.as_str() {
                    "LC" => arg.to_lowercase(),
                    "UC" => arg.to_uppercase(),
                    "LCFIRST" => first_case(&arg, false),
                    _ => first_case(&arg, true),
                };
            }
            _ => {}
        }
        if MAGIC_WORDS.contains(&upper.as_str()) && head == upper {
            self.report.miss(&format!("{{{{{head}}}}}"), inner);
            return String::new();
        }
        if colon {
            let prefix = head.to_ascii_lowercase();
            if prefix == "subst" || prefix == "safesubst" {
                return self.template_call(after_colon);
            }
            if !matches!(prefix.as_str(), "template" | "шаблон") {
                self.report.miss(&format!("{{{{{prefix}:}}}}"), inner);
                return String::new();
            }
        }

        // Reference lists: notes collect at the end on their own.
        let name = trimmed.split('|').next().unwrap_or("").trim().to_string();
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "reflist" | "references" | "refs" | "notelist"
        ) {
            self.report.ok("reference list");
            return String::new();
        }

        let body = self.templates(inner);
        if let Some(slug) = naw_markdown::transclude::template_slug(&name) {
            let body = self.image_args(&slug, &body);
            *self.report.templates.entry(slug).or_default() += 1;
            self.report.ok("template");
            return format!("{{{{{body}}}}}");
        }
        // A name the engine cannot spell, such as `Status/Legend`, calls
        // the template under its address instead.
        let bare = name
            .split_once(':')
            .filter(|(p, _)| matches!(p.trim().to_lowercase().as_str(), "template" | "шаблон"))
            .map_or(name.as_str(), |(_, rest)| rest);
        match crate::slug::page_slug(bare) {
            Some(slug) => {
                *self.report.templates.entry(slug.clone()).or_default() += 1;
                self.report.ok("template renamed");
                // The name as written may be gone from the body, when what
                // was inside it changed; then the call stays as it is.
                match body.find(name.as_str()) {
                    Some(at) => {
                        format!("{{{{{}{slug}{}}}}}", &body[..at], &body[at + name.len()..])
                    }
                    None => format!("{{{{{body}}}}}"),
                }
            }
            None => {
                self.report.miss("template name", &name);
                format!("{{{{{body}}}}}")
            }
        }
    }

    /// A call's picture arguments, `image1 = Bao 2.0 New.jpg`, as the
    /// engine's file addresses, `image1 = image:bao-2-0-new.jpg`.
    fn image_args(&mut self, slug: &str, body: &str) -> String {
        let Some(params) = self.options.image_params.get(slug) else {
            return body.to_string();
        };
        let mut parts = split_top(body, "|");
        for part in parts.iter_mut().skip(1) {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            let name = value.trim();
            if !params.contains(key.trim()) || name.is_empty() || name.contains(['[', '{', '<']) {
                continue;
            }
            let name = strip_file_prefix(name, self.options).unwrap_or(name);
            let Some((prefix, file)) = file_name(name) else {
                continue;
            };
            self.report.files.insert(name.to_string());
            self.report.ok("picture argument");
            let lead = &value[..value.len() - value.trim_start().len()];
            let tail = &value[value.trim_end().len()..];
            *part = format!("{key}={lead}{prefix}:{file}{tail}");
        }
        parts.join("|")
    }

    /// `{{#tag:ref|text|name=x}}` and `{{#tag:tabber|...}}`, the forms
    /// FANDOM pages use to build a tag around template output.
    fn tag_function(&mut self, args: &str, whole: &str) -> String {
        let parts = split_top(args, "|");
        let tag = parts[0].trim().to_ascii_lowercase();
        let content = parts.get(1).cloned().unwrap_or_default();
        let attrs: Vec<String> = parts
            .iter()
            .skip(2)
            .filter_map(|p| {
                p.split_once('=')
                    .map(|(k, v)| format!("{}=\"{}\"", k.trim(), v.trim()))
            })
            .collect();
        match tag.as_str() {
            "ref" => {
                let content = self.templates(&content);
                let label = self.note(&attrs.join(" "), Some(&content));
                self.report.ok("footnote");
                self.put(format!("[^{label}]"))
            }
            "tabber" => {
                let content = content.replace("{{!}}", "|");
                let text = self.block_tags(&format!("<tabber>{content}</tabber>"));
                self.templates(&text)
            }
            _ => {
                self.report.miss("{{#tag}}", whole);
                String::new()
            }
        }
    }

    /// `{{#ev:youtube|id|width|align|caption}}` as a link to the video.
    fn embed_video(&mut self, args: &str, whole: &str) -> String {
        let parts: Vec<&str> = args.split('|').map(str::trim).collect();
        let service = parts
            .first()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let id = parts.get(1).copied().unwrap_or("");
        let caption = parts.get(4).copied().filter(|c| !c.is_empty());
        let url = match service.as_str() {
            "youtube" | "youtubenocookie" if !id.is_empty() => {
                if id.starts_with("http") {
                    id.to_string()
                } else {
                    format!("https://www.youtube.com/watch?v={id}")
                }
            }
            _ => {
                self.report.miss("embedded video", whole);
                return String::new();
            }
        };
        self.report.ok("embedded video");
        let text = caption.unwrap_or("YouTube video");
        format!("[{}]({url})", escape_text(&plain(text)))
    }

    // -----------------------------------------------------------------
    // Pass 3: lines
    // -----------------------------------------------------------------

    fn blocks(&mut self, text: &str) -> String {
        let lines: Vec<&str> = text.split('\n').collect();
        let mut out = String::with_capacity(text.len() + text.len() / 8);
        let mut depth: i32 = 0;
        let mut prev = Prev::Blank;
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();
            if depth > 0 {
                // Inside a template call spread over lines: its arguments
                // are not lines of the page.
                out.push_str(&self.inline(line, false));
                out.push('\n');
                depth = (depth + brace_delta(line)).max(0);
                i += 1;
                continue;
            }
            if trimmed.starts_with("{|") {
                let end = table_end(&lines, i);
                let table = self.table(&lines[i..end]);
                blank_before(&mut out);
                out.push_str(&table);
                out.push_str("\n\n");
                prev = Prev::Blank;
                i = end;
                continue;
            }
            if trimmed.is_empty() {
                out.push('\n');
                prev = Prev::Blank;
                i += 1;
                continue;
            }
            if trimmed.starts_with(BLOCK_OPEN) {
                blank_before(&mut out);
                out.push_str(trimmed);
                out.push_str("\n\n");
                prev = Prev::Blank;
                i += 1;
                continue;
            }
            if let Some((level, title)) = heading(trimmed) {
                self.report.ok("heading");
                blank_before(&mut out);
                let title = self.inline(title, true);
                out.push_str(&format!("{} {}\n\n", "#".repeat(level), title.trim()));
                prev = Prev::Blank;
                i += 1;
                continue;
            }
            if trimmed.len() >= 4 && trimmed.chars().all(|c| c == '-') {
                self.report.ok("rule");
                blank_before(&mut out);
                out.push_str("---\n\n");
                prev = Prev::Blank;
                i += 1;
                continue;
            }
            if line.starts_with(['*', '#', ':', ';']) {
                let item = self.list_line(line, prev);
                if item.starts_with('\n') && !matches!(prev, Prev::Blank) {
                    // A definition list's term starts a paragraph of its own.
                    blank_before(&mut out);
                    out.push_str(&item[1..]);
                } else if !matches!(prev, Prev::List | Prev::Blank) && !item.starts_with(": ") {
                    blank_before(&mut out);
                    out.push_str(&item);
                } else {
                    out.push_str(&item);
                }
                out.push('\n');
                prev = if line.starts_with(':') && !matches!(prev, Prev::List | Prev::Term) {
                    Prev::Para
                } else if line.starts_with(';') {
                    Prev::Term
                } else {
                    Prev::List
                };
                depth = (depth + brace_delta(line)).max(0);
                i += 1;
                continue;
            }
            if line.starts_with(' ') && !trimmed.starts_with(['<', '{', '|', '!', OPEN]) {
                // A space at the start of a line is preformatted text.
                let mut code = Vec::new();
                while i < lines.len() && lines[i].starts_with(' ') && !lines[i].trim().is_empty() {
                    code.push(&lines[i][1..]);
                    i += 1;
                }
                self.report.ok("preformatted text");
                blank_before(&mut out);
                out.push_str(&fence("", &code.join("\n")));
                out.push_str("\n\n");
                prev = Prev::Blank;
                continue;
            }
            if let Some(html) = self.block_html(trimmed) {
                blank_before(&mut out);
                out.push_str(&html);
                out.push_str("\n\n");
                prev = Prev::Blank;
                depth = (depth + brace_delta(line)).max(0);
                i += 1;
                continue;
            }
            // A paragraph line.
            if matches!(prev, Prev::List | Prev::Term) {
                blank_before(&mut out);
            }
            let converted = self.inline(trimmed, true);
            out.push_str(&escape_line_start(&converted));
            out.push('\n');
            prev = Prev::Para;
            depth = (depth + brace_delta(line)).max(0);
            i += 1;
        }
        out
    }

    fn list_line(&mut self, line: &str, prev: Prev) -> String {
        let prefix: String = line
            .chars()
            .take_while(|c| matches!(c, '*' | '#' | ':' | ';'))
            .collect();
        let rest = line[prefix.len()..].trim();
        let last = prefix.chars().last().unwrap_or('*');
        let indent: String = prefix
            .chars()
            .take(prefix.chars().count() - 1)
            .map(|c| if c == '#' { "   " } else { "  " })
            .collect();
        match last {
            '*' => {
                self.report.ok("list");
                format!("{indent}- {}", self.inline(rest, true))
            }
            '#' => {
                self.report.ok("list");
                format!("{indent}1. {}", self.inline(rest, true))
            }
            ';' => {
                self.report.ok("definition");
                let (term, definition) = match split_definition(rest) {
                    Some((t, d)) => (t, Some(d)),
                    None => (rest, None),
                };
                if prefix.len() > 1 {
                    let mut s = format!("{indent}- **{}**", self.inline(term.trim(), true));
                    if let Some(d) = definition {
                        s.push_str(&format!(": {}", self.inline(d.trim(), true)));
                    }
                    return s;
                }
                let mut s = format!("\n{}", self.inline(term.trim(), true));
                if let Some(d) = definition {
                    s.push_str(&format!("\n: {}", self.inline(d.trim(), true)));
                }
                s
            }
            _ => {
                if prefix.chars().all(|c| c == ':') {
                    if prefix.len() == 1 && matches!(prev, Prev::Term) {
                        self.report.ok("definition");
                        return format!(": {}", self.inline(rest, true));
                    }
                    // Indentation without a list: a paragraph of its own.
                    self.report.ok("indent");
                    return format!("\n{}", escape_line_start(&self.inline(rest, true)));
                }
                // `*:` and `#:` continue the item above.
                self.report.ok("list");
                format!("{indent}  {}", self.inline(rest, true))
            }
        }
    }

    /// A line of block HTML, such as `<div class="x">` alone or around its
    /// text, laid out so the Markdown between the tags still renders.
    fn block_html(&mut self, trimmed: &str) -> Option<String> {
        let name = tag_name(trimmed)?;
        if !BLOCK_TAGS.contains(&name.as_str()) {
            return None;
        }
        let mut pieces = Vec::new();
        let mut rest = trimmed;
        // Leading tags, each on a line of its own.
        while let Some(name) = tag_name(rest).filter(|n| BLOCK_TAGS.contains(&n.as_str())) {
            let end = rest.find('>')? + 1;
            pieces.push(self.html_tag(&rest[..end], &name));
            rest = rest[end..].trim_start();
        }
        // Trailing closing tags.
        let mut tail = Vec::new();
        loop {
            let t = rest.trim_end();
            let Some(open) = t.rfind("</") else { break };
            if !t.ends_with('>') {
                break;
            }
            let tag = &t[open..];
            let Some(name) = tag_name(tag).filter(|n| BLOCK_TAGS.contains(&n.as_str())) else {
                break;
            };
            tail.push(self.html_tag(tag, &name));
            rest = &t[..open];
        }
        let middle = rest.trim();
        if !middle.is_empty() {
            pieces.push(String::new());
            pieces.push(escape_line_start(&self.inline(middle, true)));
            pieces.push(String::new());
        }
        pieces.extend(tail.into_iter().rev());
        Some(pieces.join("\n"))
    }

    /// A wikitext table, as a Markdown table when it is plain enough and as
    /// HTML when it spans cells or holds blocks.
    fn table(&mut self, lines: &[&str]) -> String {
        let mut caption: Option<String> = None;
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let mut current: Vec<Cell> = Vec::new();
        let mut nested = 0;
        let mut depth: i32 = 0;
        for line in &lines[1..] {
            let t = line.trim_start();
            if nested == 0 && depth == 0 && t.starts_with("|}") {
                break;
            }
            if nested > 0 || depth > 0 || (t.starts_with("{|") && !current.is_empty()) {
                if t.starts_with("{|") {
                    nested += 1;
                }
                if t.starts_with("|}") && nested > 0 {
                    nested -= 1;
                }
                if let Some(cell) = current.last_mut() {
                    cell.content.push('\n');
                    cell.content.push_str(line);
                }
                depth = (depth + brace_delta(line)).max(0);
                continue;
            }
            if let Some(c) = t.strip_prefix("|+") {
                caption = Some(c.trim().to_string());
            } else if t.starts_with("|-") {
                if !current.is_empty() {
                    rows.push(std::mem::take(&mut current));
                }
            } else if let Some(c) = t.strip_prefix('!') {
                for part in split_cells(c, true) {
                    current.push(Cell::parse(&part, true));
                }
            } else if let Some(c) = t.strip_prefix('|') {
                for part in split_cells(c, false) {
                    current.push(Cell::parse(&part, false));
                }
            } else if let Some(cell) = current.last_mut() {
                cell.content.push('\n');
                cell.content.push_str(line);
            }
            depth = (depth + brace_delta(line)).max(0);
        }
        if !current.is_empty() {
            rows.push(current);
        }
        rows.retain(|r| !r.is_empty());
        if rows.is_empty() {
            self.report.miss("empty table", lines[0]);
            return String::new();
        }
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let plain_enough = rows.iter().flatten().all(|cell| {
            !cell.spans()
                && !cell.content.trim().contains('\n')
                && !cell.content.contains(BLOCK_OPEN)
        });
        if plain_enough {
            self.report.ok("table");
            let mut out = String::new();
            if let Some(c) = &caption {
                out.push_str(&format!("**{}**\n\n", self.inline(c, true)));
            }
            if !rows[0].iter().all(|c| c.header) {
                self.report.ok("table without a header row");
            }
            for (n, row) in rows.iter().enumerate() {
                out.push('|');
                for col in 0..columns {
                    let text = match row.get(col) {
                        Some(cell) => {
                            let text = self.inline(cell.content.trim(), true);
                            if cell.header && n > 0 && !text.is_empty() {
                                format!("**{text}**")
                            } else {
                                text
                            }
                        }
                        None => String::new(),
                    };
                    out.push_str(&format!(" {} |", text.replace('\n', " ")));
                }
                out.push('\n');
                if n == 0 {
                    out.push('|');
                    out.push_str(&"---|".repeat(columns));
                    out.push('\n');
                }
            }
            return out.trim_end().to_string();
        }
        self.report.ok("table as HTML");
        let mut out = String::from("<table>\n");
        if let Some(c) = &caption {
            out.push_str(&format!(
                "<caption>\n\n{}\n\n</caption>\n",
                self.inline(c, true)
            ));
        }
        for row in &rows {
            out.push_str("<tr>\n");
            for cell in row {
                let tag = if cell.header { "th" } else { "td" };
                let attrs = cell.kept_attrs();
                let body = self.blocks(cell.content.trim());
                out.push_str(&format!("<{tag}{attrs}>\n\n{}\n\n</{tag}>\n", body.trim()));
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</table>");
        out
    }

    // -----------------------------------------------------------------
    // Pass 4: inside a line
    // -----------------------------------------------------------------

    /// Converts the markup inside a line. `escape` protects plain text from
    /// reading as Markdown; template arguments go unescaped, since the
    /// template's own text decides what they become.
    fn inline(&mut self, s: &str, escape: bool) -> String {
        let mut line = Line {
            out: String::with_capacity(s.len() + 16),
            text: String::new(),
            pending: String::new(),
            stack: Vec::new(),
            escape,
        };
        let bytes = s.as_bytes();
        let mut braces = Braces::new(s);
        let mut i = 0;
        while i < bytes.len() {
            let rest = &s[i..];
            if rest.starts_with("[[")
                && let Some(end) = braces.link(i)
            {
                let inner = &s[i + 2..end - 2];
                let trail: String = s[end..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase())
                    .collect();
                let link = self.wikilink(inner, &trail);
                line.token(&link);
                i = end + trail.len();
                continue;
            }
            if rest.starts_with("{{{")
                && let Some(end) = braces.triple(i)
            {
                line.token(&s[i..end]);
                i = end;
                continue;
            }
            if rest.starts_with("{{")
                && let Some(end) = braces.double(i)
            {
                let inner = self.inline(&s[i + 2..end - 2], false);
                line.token(&format!("{{{{{inner}}}}}"));
                i = end;
                continue;
            }
            if rest.starts_with('[')
                && starts_with_scheme(&rest[1..])
                && let Some(close) = rest.find(']')
            {
                let inner = &rest[1..close];
                let link = self.external_link(inner);
                line.token(&link);
                i += close + 1;
                continue;
            }
            if (rest.starts_with("http://") || rest.starts_with("https://"))
                && (i == 0 || !s[..i].ends_with(|c: char| c.is_alphanumeric()))
            {
                let url = bare_url(rest);
                line.token(url);
                i += url.len();
                continue;
            }
            if rest.starts_with('<')
                && let Some((end, name, closing)) = html_tag_at(rest)
            {
                let replacement = self.inline_tag(&rest[..end], &name, closing);
                line.token(&replacement);
                i += end;
                continue;
            }
            if (rest.starts_with(OPEN) || rest.starts_with(BLOCK_OPEN))
                && let Some(end) = rest.find(CLOSE)
            {
                let len = end + CLOSE.len_utf8();
                line.token(&rest[..len]);
                i += len;
                continue;
            }
            if rest.starts_with("''") {
                let run = rest.chars().take_while(|c| *c == '\'').count();
                line.apostrophes(run);
                i += run;
                continue;
            }
            let c = rest.chars().next().unwrap_or(' ');
            line.text.push(c);
            i += c.len_utf8();
        }
        line.finish()
    }

    fn wikilink(&mut self, inner: &str, trail: &str) -> String {
        let parts = split_top(inner, "|");
        let target_raw = parts[0].trim();
        let leading_colon = target_raw.starts_with(':');
        let target = target_raw.trim_start_matches(':').trim();
        let label = (parts.len() > 1).then(|| parts[1..].join("|"));

        if let Some((ns, rest)) = target.split_once(':') {
            let ns_lower = ns.trim().to_lowercase();
            let rest = rest.trim();
            if self.is_category(&ns_lower) {
                return match category_key(rest) {
                    Some(key) if leading_colon => {
                        self.report.ok("category link");
                        let text = label.unwrap_or_else(|| format!("Category:{rest}"));
                        format!("[[:Category:{key}|{}]]", self.inline(text.trim(), true))
                    }
                    Some(key) => {
                        self.report.ok("category");
                        format!("[[Category:{key}]]")
                    }
                    None => {
                        self.report.miss("category name", target);
                        String::new()
                    }
                };
            }
            if self.is_file(&ns_lower) || ns_lower == "media" {
                return if leading_colon || ns_lower == "media" {
                    let text = label.unwrap_or_else(|| rest.to_string());
                    match file_name(rest) {
                        Some((prefix, name)) => {
                            self.report.files.insert(rest.to_string());
                            self.report.ok("file link");
                            format!("[{}]({prefix}:{name})", escape_text(&plain(&text)))
                        }
                        None => {
                            self.report.miss("file name", rest);
                            escape_text(&plain(&text))
                        }
                    }
                } else {
                    self.file_embed(rest, &parts[1..])
                };
            }
            match ns_lower.as_str() {
                "wikipedia" | "wp" => {
                    self.report.ok("interwiki link");
                    let text = label.unwrap_or_else(|| rest.to_string());
                    return format!(
                        "[{}](https://en.wikipedia.org/wiki/{})",
                        self.inline(text.trim(), true),
                        url_path(rest)
                    );
                }
                "w" => {
                    self.report.ok("interwiki link");
                    let (url, shown) = match rest.strip_prefix("c:").and_then(|r| r.split_once(':'))
                    {
                        Some((wiki, page)) => (
                            format!("https://{}.fandom.com/wiki/{}", wiki.trim(), url_path(page)),
                            page.to_string(),
                        ),
                        None => (
                            format!("https://community.fandom.com/wiki/{}", url_path(rest)),
                            rest.to_string(),
                        ),
                    };
                    let text = label.unwrap_or(shown);
                    return format!("[{}]({url})", self.inline(text.trim(), true));
                }
                _ => {}
            }
            if !leading_colon && is_language_code(&ns_lower) {
                self.report.ok("language link dropped");
                return String::new();
            }
            if OTHER_NAMESPACES.contains(&ns_lower.as_str()) || is_language_code(&ns_lower) {
                self.report.miss(&format!("link to {ns_lower}:"), target);
                let text = label.unwrap_or_else(|| rest.to_string());
                return self.inline(text.trim(), true) + trail;
            }
        }

        // An ordinary page.
        let (title, section) = match target.split_once('#') {
            Some((t, s)) => (t.trim(), Some(s.trim())),
            None => (target, None),
        };
        let shown = match &label {
            Some(l) if l.trim().is_empty() => pipe_trick(title),
            Some(l) => l.trim().to_string(),
            None => match section {
                Some(s) if !title.is_empty() => format!("{title}#{s}"),
                Some(s) => s.to_string(),
                None => title.to_string(),
            },
        };
        let shown = format!("{shown}{trail}");
        let anchor = section
            .map(|s| format!("#{}", naw_markdown::slugify(s)))
            .unwrap_or_default();
        if title.is_empty() {
            self.report.ok("section link");
            return format!("[{}]({anchor})", self.inline(&shown, true));
        }
        let Some(slug) = page_slug(title) else {
            self.report.miss("page name", title);
            return self.inline(&shown, true);
        };
        self.report.ok("link");
        self.report.links.insert(slug.clone());
        let href = format!("{slug}{anchor}");
        let text = self.inline(&shown, true);
        if text == href {
            format!("[[{href}]]")
        } else {
            format!("[[{href}|{text}]]")
        }
    }

    fn file_embed(&mut self, name: &str, options: &[String]) -> String {
        let mut alt = None;
        let mut caption = None;
        for option in options {
            let o = option.trim();
            let lower = o.to_ascii_lowercase();
            if let Some(a) = lower.strip_prefix("alt=") {
                alt = Some(o[o.len() - a.len()..].to_string());
            } else if !is_image_option(&lower) {
                caption = Some(o.to_string());
            }
        }
        let text = plain(&alt.or(caption).unwrap_or_default());
        match self.file_markdown(name, &text, false) {
            Some(md) => md,
            None => {
                self.report.miss("file name", name);
                String::new()
            }
        }
    }

    /// `![caption](image:name.png)`, recording the file.
    fn file_markdown(&mut self, name: &str, caption: &str, gallery: bool) -> Option<String> {
        let (prefix, file) = file_name(name)?;
        self.report.files.insert(name.trim().to_string());
        self.report.ok(if gallery {
            "gallery picture"
        } else {
            "picture"
        });
        let caption = caption.replace(['[', ']'], "");
        Some(format!("![{}]({prefix}:{file})", caption.trim()))
    }

    fn external_link(&mut self, inner: &str) -> String {
        let (url, text) = match inner.split_once([' ', '\t']) {
            Some((u, t)) => (u, t.trim()),
            None => (inner, ""),
        };
        let url = if let Some(rest) = url.strip_prefix("//") {
            format!("https://{rest}")
        } else {
            url.to_string()
        };
        let url = url.replace('(', "%28").replace(')', "%29");
        self.report.ok("external link");
        if text.is_empty() {
            format!("<{url}>")
        } else {
            format!("[{}]({url})", self.inline(text, true).replace(']', "\\]"))
        }
    }

    fn inline_tag(&mut self, tag: &str, name: &str, closing: bool) -> String {
        match name {
            "br" => "<br>".to_string(),
            "s" | "strike" | "del" => {
                self.report.ok("strikethrough");
                "~~".to_string()
            }
            "center" | "font" | "big" | "div" | "blockquote" | "table" => self.html_tag(tag, name),
            // The engine's templates know these two.
            "noinclude" | "includeonly" => {
                self.report.ok("template section");
                if closing {
                    format!("</{name}>")
                } else {
                    format!("<{name}>")
                }
            }
            _ if naw_markdown::AUTHOR_TAGS.contains(&name) => {
                self.report.ok("html");
                if closing {
                    format!("</{name}>")
                } else {
                    tag.to_string()
                }
            }
            _ => {
                self.report.miss(&format!("tag <{name}>"), tag);
                String::new()
            }
        }
    }

    /// A block or styling tag as the engine allows it.
    fn html_tag(&mut self, tag: &str, name: &str) -> String {
        let closing = tag.starts_with("</");
        let (open, close) = match name {
            "center" => ("<div style=\"text-align: center\">".to_string(), "</div>"),
            "big" => ("<span style=\"font-size: larger\">".to_string(), "</span>"),
            "font" => {
                let color = attr_value(tag, "color")
                    .filter(|c| c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '#'));
                match color {
                    Some(c) => (format!("<span style=\"color: {c}\">"), "</span>"),
                    None => ("<span>".to_string(), "</span>"),
                }
            }
            _ => {
                self.report.ok("html");
                return if closing {
                    format!("</{name}>")
                } else {
                    tag.to_string()
                };
            }
        };
        self.report.ok("html");
        if closing { close.to_string() } else { open }
    }

    fn is_category(&self, ns: &str) -> bool {
        matches!(ns, "category" | "категория")
            || self
                .options
                .category_names
                .iter()
                .any(|n| n.to_lowercase() == ns)
    }

    fn is_file(&self, ns: &str) -> bool {
        matches!(ns, "file" | "image" | "файл" | "изображение")
            || self
                .options
                .file_names
                .iter()
                .any(|n| n.to_lowercase() == ns)
    }
}

/// The category's key here, from its name on the source wiki.
pub(crate) fn category_key(name: &str) -> Option<String> {
    let name = name.split('|').next().unwrap_or(name).replace('_', " ");
    naw_markdown::categories::key(name.trim())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    Blank,
    Para,
    List,
    /// A definition list's term, which `:` lines may follow.
    Term,
}

/// One line being converted: what is written, plain text not yet written,
/// emphasis opened but not yet written, and what is open.
struct Line {
    out: String,
    text: String,
    pending: String,
    stack: Vec<Emphasis>,
    escape: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Emphasis {
    Bold,
    Italic,
}

impl Emphasis {
    fn marker(self) -> &'static str {
        match self {
            Emphasis::Bold => "**",
            Emphasis::Italic => "*",
        }
    }
}

impl Line {
    fn flush(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        let text = if self.escape {
            escape_text(&text)
        } else {
            text
        };
        if self.pending.is_empty() {
            self.out.push_str(&text);
        } else {
            // An opening marker cannot be followed by a space.
            let start = text.len() - text.trim_start().len();
            self.out.push_str(&text[..start]);
            self.out.push_str(&std::mem::take(&mut self.pending));
            self.out.push_str(&text[start..]);
        }
    }

    fn token(&mut self, s: &str) {
        self.flush();
        if !self.pending.is_empty() && !s.is_empty() {
            self.out.push_str(&std::mem::take(&mut self.pending));
        }
        self.out.push_str(s);
    }

    fn open(&mut self, e: Emphasis) {
        self.stack.push(e);
        self.pending.push_str(e.marker());
    }

    fn close(&mut self, e: Emphasis) {
        // Close everything above it, then reopen those, so the markers nest.
        let Some(at) = self.stack.iter().rposition(|x| *x == e) else {
            return;
        };
        let above: Vec<Emphasis> = self.stack.drain(at..).skip(1).collect();
        for x in above.iter().rev() {
            self.write_close(*x);
        }
        self.write_close(e);
        for x in above {
            self.open(x);
        }
    }

    fn write_close(&mut self, e: Emphasis) {
        let marker = e.marker();
        if self.pending.ends_with(marker) {
            // Opened and closed with nothing between: write neither.
            let len = self.pending.len() - marker.len();
            self.pending.truncate(len);
            return;
        }
        // A closing marker cannot follow a space.
        let trimmed_len = self.out.trim_end().len();
        let tail = self.out[trimmed_len..].to_string();
        self.out.truncate(trimmed_len);
        self.out.push_str(marker);
        self.out.push_str(&tail);
    }

    fn toggle(&mut self, e: Emphasis) {
        if self.stack.contains(&e) {
            self.close(e);
        } else {
            self.open(e);
        }
    }

    /// A run of apostrophes: `''` italic, `'''` bold, `'''''` both.
    fn apostrophes(&mut self, run: usize) {
        let (literal, kind) = match run {
            2 => (0, 2),
            3 => (0, 3),
            4 => (1, 3),
            n => (n - 5, 5),
        };
        self.text.push_str(&"'".repeat(literal));
        self.flush();
        match kind {
            2 => self.toggle(Emphasis::Italic),
            3 => self.toggle(Emphasis::Bold),
            _ => {
                let bold = self.stack.contains(&Emphasis::Bold);
                let italic = self.stack.contains(&Emphasis::Italic);
                match (bold, italic) {
                    (true, true) => {
                        let top = *self.stack.last().expect("both open");
                        let other = if top == Emphasis::Bold {
                            Emphasis::Italic
                        } else {
                            Emphasis::Bold
                        };
                        self.close(top);
                        self.close(other);
                    }
                    (true, false) => {
                        self.close(Emphasis::Bold);
                        self.open(Emphasis::Italic);
                    }
                    (false, true) => {
                        self.close(Emphasis::Italic);
                        self.open(Emphasis::Bold);
                    }
                    (false, false) => {
                        self.open(Emphasis::Bold);
                        self.open(Emphasis::Italic);
                    }
                }
            }
        }
    }

    fn finish(mut self) -> String {
        self.flush();
        while let Some(e) = self.stack.pop() {
            self.write_close(e);
        }
        self.pending.clear();
        self.out
    }
}

/// A table cell: whether it is a header, its attributes and its wikitext.
struct Cell {
    header: bool,
    attrs: String,
    content: String,
}

impl Cell {
    fn parse(part: &str, header: bool) -> Cell {
        // `style="x" | text`: attributes before a single pipe.
        let pieces = split_top(part, "|");
        if pieces.len() >= 2 && looks_like_attributes(&pieces[0]) {
            return Cell {
                header,
                attrs: pieces[0].trim().to_string(),
                content: pieces[1..].join("|"),
            };
        }
        Cell {
            header,
            attrs: String::new(),
            content: part.to_string(),
        }
    }

    fn spans(&self) -> bool {
        let lower = self.attrs.to_ascii_lowercase();
        lower.contains("rowspan") || lower.contains("colspan")
    }

    /// The attributes that carry over to an HTML cell.
    fn kept_attrs(&self) -> String {
        let mut out = String::new();
        for name in ["rowspan", "colspan"] {
            if let Some(v) =
                attr_value(&self.attrs, name).filter(|v| v.chars().all(|c| c.is_ascii_digit()))
            {
                out.push_str(&format!(" {name}=\"{v}\""));
            }
        }
        if let Some(style) = attr_value(&self.attrs, "style") {
            out.push_str(&format!(" style=\"{}\"", style.replace('"', "'")));
        }
        out
    }
}

/// Variables MediaWiki fills in that have nothing to stand for here.
const MAGIC_WORDS: &[&str] = &[
    "CURRENTYEAR",
    "CURRENTMONTH",
    "CURRENTMONTHNAME",
    "CURRENTDAY",
    "CURRENTDAYNAME",
    "CURRENTTIME",
    "CURRENTTIMESTAMP",
    "CURRENTWEEK",
    "LOCALYEAR",
    "LOCALMONTH",
    "LOCALDAY",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "SITENAME",
    "SERVER",
    "SERVERNAME",
    "SCRIPTPATH",
    "NAMESPACE",
    "NAMESPACENUMBER",
    "TALKPAGENAME",
    "TALKSPACE",
    "SUBJECTSPACE",
    "REVISIONID",
    "REVISIONDAY",
    "REVISIONMONTH",
    "REVISIONYEAR",
    "REVISIONTIMESTAMP",
    "REVISIONUSER",
    "NUMBEROFARTICLES",
    "NUMBEROFPAGES",
    "NUMBEROFFILES",
    "NUMBEROFEDITS",
    "NUMBEROFUSERS",
    "NUMBEROFACTIVEUSERS",
    "NUMBEROFADMINS",
    "CONTENTLANGUAGE",
    "DIRMARK",
    "PAGEID",
];

/// Block tags that get lines of their own.
const BLOCK_TAGS: &[&str] = &[
    "div",
    "center",
    "blockquote",
    "table",
    "figure",
    "section",
    "aside",
    "details",
    "ul",
    "ol",
    "dl",
    "header",
    "footer",
    "article",
    "p",
];

/// Namespaces whose links have no page here.
const OTHER_NAMESPACES: &[&str] = &[
    "user",
    "user talk",
    "talk",
    "special",
    "help",
    "help talk",
    "project",
    "project talk",
    "message wall",
    "message wall greeting",
    "board",
    "board thread",
    "thread",
    "blog",
    "user blog",
    "user blog comment",
    "forum",
    "mediawiki",
    "module",
    "template",
    "template talk",
    "category talk",
    "file talk",
    "map",
    "gadget",
    "участник",
    "обсуждение",
    "служебная",
    "справка",
    "модуль",
    "шаблон",
];

fn is_language_code(ns: &str) -> bool {
    let base = ns.split('-').next().unwrap_or(ns);
    (2..=3).contains(&base.len())
        && base.chars().all(|c| c.is_ascii_lowercase())
        && ns.len() <= 10
        && !matches!(ns, "w" | "wp" | "c")
}

fn is_image_option(o: &str) -> bool {
    matches!(
        o,
        "thumb"
            | "thumbnail"
            | "frame"
            | "framed"
            | "frameless"
            | "border"
            | "left"
            | "right"
            | "center"
            | "centre"
            | "none"
            | "baseline"
            | "sub"
            | "super"
            | "top"
            | "text-top"
            | "middle"
            | "bottom"
            | "text-bottom"
            | "upright"
            | "loop"
            | "autoplay"
            | "muted"
    ) || o.starts_with("upright=")
        || o.starts_with("link=")
        || o.starts_with("page=")
        || o.starts_with("class=")
        || o.starts_with("lang=")
        || o.starts_with("start=")
        || o.starts_with("end=")
        || (o.ends_with("px")
            && o[..o.len() - 2]
                .chars()
                .all(|c| c.is_ascii_digit() || c == 'x'))
}

/// `[[Bao (VTuber)|]]` shows `Bao`.
fn pipe_trick(title: &str) -> String {
    let t = title.split_once(':').map_or(title, |(_, r)| r);
    let t = match t.find(" (") {
        Some(at) if t.ends_with(')') => &t[..at],
        _ => t,
    };
    t.split_once(", ").map_or(t, |(a, _)| a).to_string()
}

fn first_case(s: &str, upper: bool) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if upper => c.to_uppercase().collect::<String>() + chars.as_str(),
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn strip_file_prefix<'a>(s: &'a str, options: &Options) -> Option<&'a str> {
    let (ns, rest) = s.split_once(':')?;
    let ns = ns.trim().to_lowercase();
    (matches!(ns.as_str(), "file" | "image" | "файл" | "изображение")
        || options.file_names.iter().any(|n| n.to_lowercase() == ns))
    .then_some(rest.trim())
}

// ---------------------------------------------------------------------
// Scanning helpers
// ---------------------------------------------------------------------

/// A tag found by [`next_tag`].
struct Tag {
    name: String,
    attrs: String,
    start: usize,
    content: std::ops::Range<usize>,
    end: usize,
    /// `<x/>` with no content.
    empty: bool,
    /// Whether it sits inside a line of text.
    inline: bool,
}

/// The first of `names` in `text`, with its content up to the matching
/// closing tag (or the end of the text when it never closes).
fn next_tag(text: &str, names: &[&str]) -> Option<Tag> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find('<') {
        let start = from + at;
        let after = &lower[start + 1..];
        let found = names.iter().find(|n| {
            after.starts_with(**n)
                && after[n.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c == '>' || c == '/' || c.is_whitespace())
        });
        let Some(name) = found else {
            from = start + 1;
            continue;
        };
        let gt = text[start..].find('>')?;
        let open_end = start + gt + 1;
        let attrs = text[start + 1 + name.len()..open_end - 1]
            .trim_end_matches('/')
            .trim()
            .to_string();
        let inline = !text[..start].ends_with('\n') && start != 0;
        if text[..open_end].ends_with("/>") {
            return Some(Tag {
                name: name.to_string(),
                attrs,
                start,
                content: open_end..open_end,
                end: open_end,
                empty: true,
                inline,
            });
        }
        let close = format!("</{name}");
        let (content_end, end) = match lower[open_end..].find(&close) {
            Some(c) => {
                let cs = open_end + c;
                let ce = text[cs..].find('>').map_or(text.len(), |g| cs + g + 1);
                (cs, ce)
            }
            None => (text.len(), text.len()),
        };
        return Some(Tag {
            name: name.to_string(),
            attrs,
            start,
            content: open_end..content_end,
            end,
            empty: false,
            inline,
        });
    }
    None
}

/// An attribute's value in `name=value`, `name="value"` or `name='value'`.
fn attr_value(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let start = from + at;
        let before_ok = start == 0
            || !lower[..start]
                .chars()
                .last()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-');
        let rest = attrs[start + name.len()..].trim_start();
        if before_ok && let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start();
            let value = if let Some(r) = rest.strip_prefix('"') {
                r.split('"').next().unwrap_or("")
            } else if let Some(r) = rest.strip_prefix('\'') {
                r.split('\'').next().unwrap_or("")
            } else {
                rest.split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                    .next()
                    .unwrap_or("")
            };
            return Some(value.to_string());
        }
        from = start + name.len();
    }
    None
}

/// Whether `word` stands alone among the attributes, as `inline` does.
fn has_word(attrs: &str, word: &str) -> bool {
    attrs
        .split(|c: char| c.is_whitespace() || c == '=')
        .any(|w| w.eq_ignore_ascii_case(word))
}

fn looks_like_attributes(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.contains('=')
        && !t.contains("[[")
        && !t.contains("{{")
        && !t.contains(OPEN)
        && t.split('=').next().is_some_and(|k| {
            let k = k.trim();
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
        })
}

/// Splits a table line into cells on `||` (and `!!` on a header line),
/// outside links and templates.
fn split_cells(line: &str, header: bool) -> Vec<String> {
    let mut cells = Vec::new();
    for part in split_top(line, "||") {
        if header {
            cells.extend(split_top(&part, "!!"));
        } else {
            cells.push(part);
        }
    }
    cells
}

/// Splits on `sep` outside `[[...]]`, `{{...}}` and stashed pieces.
fn split_top(s: &str, sep: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut links = 0usize;
    let mut braces = 0usize;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with("[[") {
            links += 1;
            current.push_str("[[");
            i += 2;
            continue;
        }
        if rest.starts_with("]]") && links > 0 {
            links -= 1;
            current.push_str("]]");
            i += 2;
            continue;
        }
        if rest.starts_with("{{") {
            braces += 1;
            current.push_str("{{");
            i += 2;
            continue;
        }
        if rest.starts_with("}}") && braces > 0 {
            braces -= 1;
            current.push_str("}}");
            i += 2;
            continue;
        }
        if links == 0 && braces == 0 && rest.starts_with(sep) {
            parts.push(std::mem::take(&mut current));
            i += sep.len();
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        current.push(c);
        i += c.len_utf8();
    }
    parts.push(current);
    parts
}

/// Splits `term : definition` on the first colon outside links, templates
/// and addresses.
fn split_definition(s: &str) -> Option<(&str, &str)> {
    let mut links = 0usize;
    let mut braces = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"[[") || bytes[i..].starts_with(b"{{") {
            if bytes[i] == b'[' {
                links += 1
            } else {
                braces += 1
            }
            i += 2;
            continue;
        }
        if bytes[i..].starts_with(b"]]") && links > 0 {
            links -= 1;
            i += 2;
            continue;
        }
        if bytes[i..].starts_with(b"}}") && braces > 0 {
            braces -= 1;
            i += 2;
            continue;
        }
        if bytes[i] == b':' && links == 0 && braces == 0 && !s[i..].starts_with("://") {
            return Some((&s[..i], &s[i + 1..]));
        }
        i += 1;
    }
    None
}

/// Templates and parameters inside each other, at most this deep; past it a
/// brace is text. MediaWiki stops far sooner, and an unbounded depth is a
/// stack overflow on a page of `{{{` repeated.
const NEST_MAX: usize = 100;

/// Finds the `}}` and `}}}` matching the braces of one text, remembering
/// every answer. Without that a run of unclosed `{{` is scanned to its end
/// from each of them, and again from each `{{{` inside: cubic, a minute for
/// a few kilobytes of braces. With it each start is scanned once.
struct Braces<'a> {
    b: &'a [u8],
    double: HashMap<usize, Option<usize>>,
    triple: HashMap<usize, Option<usize>>,
    /// Bytes the scans may still look at. Past it every brace is text, so
    /// even a page built to be slow costs a bounded amount of work.
    budget: usize,
}

impl<'a> Braces<'a> {
    fn new(s: &'a str) -> Self {
        Braces {
            b: s.as_bytes(),
            double: HashMap::new(),
            triple: HashMap::new(),
            budget: s.len().saturating_mul(32) + 4096,
        }
    }

    /// Index just past the `}}` matching the `{{` at `start`.
    fn double(&mut self, start: usize) -> Option<usize> {
        self.double_at(start, 0)
    }

    /// Index just past the `}}}` matching the `{{{` at `start`.
    fn triple(&mut self, start: usize) -> Option<usize> {
        self.triple_at(start, 0)
    }

    fn double_at(&mut self, start: usize, nest: usize) -> Option<usize> {
        if let Some(hit) = self.double.get(&start) {
            return *hit;
        }
        if nest > NEST_MAX {
            return None;
        }
        let b = self.b;
        let mut depth = 1usize;
        let mut i = start + 2;
        let mut found = None;
        while i < b.len() {
            if self.budget == 0 {
                break;
            }
            self.budget -= 1;
            if b[i..].starts_with(b"{{{")
                && let Some(end) = self.triple_at(i, nest + 1)
            {
                i = end;
                continue;
            }
            if b[i..].starts_with(b"{{") {
                depth += 1;
                i += 2;
                continue;
            }
            if b[i..].starts_with(b"}}") {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    found = Some(i);
                    break;
                }
                continue;
            }
            i += 1;
        }
        self.double.insert(start, found);
        found
    }

    /// Index just past the `]]` matching the `[[` at `start`, on its line,
    /// from the same budget as the braces.
    fn link(&mut self, start: usize) -> Option<usize> {
        let b = self.b;
        let mut depth = 0usize;
        let mut i = start;
        while i < b.len() {
            if self.budget == 0 {
                return None;
            }
            self.budget -= 1;
            if b[i..].starts_with(b"[[") {
                depth += 1;
                i += 2;
                continue;
            }
            if b[i..].starts_with(b"]]") {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return Some(i);
                }
                continue;
            }
            if b[i] == b'\n' {
                return None;
            }
            i += 1;
        }
        None
    }

    fn triple_at(&mut self, start: usize, nest: usize) -> Option<usize> {
        if let Some(hit) = self.triple.get(&start) {
            return *hit;
        }
        if nest > NEST_MAX {
            return None;
        }
        let b = self.b;
        let mut i = start + 3;
        let mut found = None;
        while i < b.len() {
            if self.budget == 0 {
                break;
            }
            self.budget -= 1;
            if b[i..].starts_with(b"}}}") {
                found = Some(i + 3);
                break;
            }
            if b[i..].starts_with(b"{{{") {
                match self.triple_at(i, nest + 1) {
                    Some(end) => {
                        i = end;
                        continue;
                    }
                    None => break,
                }
            }
            if b[i..].starts_with(b"{{") {
                match self.double_at(i, nest + 1) {
                    Some(end) => {
                        i = end;
                        continue;
                    }
                    None => break,
                }
            }
            i += 1;
        }
        self.triple.insert(start, found);
        found
    }
}

/// Template braces a line opens, less those it closes.
fn brace_delta(line: &str) -> i32 {
    line.matches("{{").count() as i32 - line.matches("}}").count() as i32
}

/// The line after the `|}` that ends the table starting at `start`.
fn table_end(lines: &[&str], start: usize) -> usize {
    let mut nested = 0;
    for (n, line) in lines.iter().enumerate().skip(start) {
        let t = line.trim_start();
        if t.starts_with("{|") {
            nested += 1;
        }
        if t.starts_with("|}") {
            nested -= 1;
            if nested == 0 {
                return n + 1;
            }
        }
    }
    lines.len()
}

/// `== Title ==` as its level and text.
fn heading(line: &str) -> Option<(usize, &str)> {
    if !line.starts_with('=') {
        return None;
    }
    let open = line.chars().take_while(|c| *c == '=').count();
    let body = line.trim_end();
    let close = body.chars().rev().take_while(|c| *c == '=').count();
    if body.len() <= open + close || close == 0 {
        return None;
    }
    let level = open.min(close).min(6);
    let text = &body[level..body.len() - level];
    let text = text.trim_matches('=').trim();
    (!text.is_empty()).then_some((level, text))
}

/// The tag name at the start of `s`, `<div ...>` or `</div>`, lowercased.
fn tag_name(s: &str) -> Option<String> {
    let rest = s.strip_prefix('<')?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase();
    (!name.is_empty() && s.contains('>')).then_some(name)
}

/// A tag at the start of `s`: its length, name and whether it closes.
fn html_tag_at(s: &str) -> Option<(usize, String, bool)> {
    let name = tag_name(s)?;
    let closing = s.starts_with("</");
    let end = s.find('>')? + 1;
    // `a < b` and `<3` are text, not tags.
    let after_name = &s[1 + usize::from(closing) + name.len()..];
    if !after_name.starts_with(['>', ' ', '/', '\t', '\n']) {
        return None;
    }
    Some((end, name, closing))
}

fn starts_with_scheme(s: &str) -> bool {
    [
        "http://", "https://", "//", "mailto:", "ftp://", "irc://", "ircs://", "news:",
    ]
    .iter()
    .any(|p| s.starts_with(p))
}

/// A bare address up to a space or a character addresses do not end on.
fn bare_url(s: &str) -> &str {
    let end = s
        .find(|c: char| {
            c.is_whitespace() || matches!(c, '<' | '>' | '[' | ']' | '{' | '}' | '|' | '"')
        })
        .unwrap_or(s.len());
    let url = &s[..end];
    url.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\''])
}

fn url_path(title: &str) -> String {
    title
        .trim()
        .replace(' ', "_")
        .replace('(', "%28")
        .replace(')', "%29")
}

// ---------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!--") {
        out.push_str(&rest[..at]);
        match rest[at..].find("-->") {
            Some(end) => rest = &rest[at + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `#REDIRECT [[Target]]` as the target's title.
fn redirect_target(text: &str) -> Option<String> {
    let t = text.trim_start();
    let lower = t.to_lowercase();
    let word = ["#redirect", "#перенаправление"]
        .iter()
        .find(|w| lower.starts_with(**w))?;
    let rest = &t[t.char_indices().nth(word.chars().count())?.0..];
    let open = rest.find("[[")?;
    let close = rest[open..].find("]]")? + open;
    let target = rest[open + 2..close].split('|').next()?.trim();
    let target = target.trim_start_matches(':');
    let target = target.split('#').next().unwrap_or(target).trim();
    (!target.is_empty()).then(|| target.to_string())
}

/// Text with the markup of links and emphasis taken out, for alt text.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut braces = Braces::new(s);
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with("[[")
            && let Some(end) = braces.link(i)
        {
            let inner = &s[i + 2..end - 2];
            out.push_str(inner.rsplit('|').next().unwrap_or(inner));
            i = end;
            continue;
        }
        if rest.starts_with("''") {
            i += rest.chars().take_while(|c| *c == '\'').count();
            continue;
        }
        if rest.starts_with('<')
            && let Some((end, _, _)) = html_tag_at(rest)
        {
            i += end;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Plain text made safe from Markdown: characters wikitext prints as they
/// are but Markdown reads as markup.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '*' | '~' | '^' | '`' => {
                out.push('\\');
                out.push(c);
            }
            '<' => out.push_str("&lt;"),
            _ => out.push(c),
        }
    }
    out
}

/// `<nowiki>` text: every character that could start markup, escaped.
fn escape_all(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c if c.is_ascii_punctuation()
                && c != '&'
                && c != '\''
                && c != '"'
                && c != '.'
                && c != ',' =>
            {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// A line of text that would start a list, a quote or a heading in Markdown
/// when it is only text in wikitext.
fn escape_line_start(line: &str) -> String {
    let lead = line.len() - line.trim_start().len();
    let (indent, body) = line.split_at(lead);
    let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 9 && body[digits..].starts_with(['.', ')']) {
        let after = &body[digits + 1..];
        if after.is_empty() || after.starts_with(' ') {
            return format!("{indent}{}\\{}", &body[..digits], &body[digits..]);
        }
    }
    if body.starts_with("- ") || body.starts_with("+ ") || body == "-" || body == "+" {
        return format!("{indent}\\{body}");
    }
    if body.starts_with('>') || body.starts_with('#') || body.starts_with("|") {
        return format!("{indent}\\{body}");
    }
    if body.starts_with("===") || body.starts_with("---") {
        return format!("{indent}\\{body}");
    }
    line.to_string()
}

fn code_span(content: &str) -> String {
    let ticks = if content.contains('`') { "``" } else { "`" };
    let pad = if content.starts_with('`') || content.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{content}{pad}{ticks}")
}

fn fence(lang: &str, content: &str) -> String {
    let content = content.trim_matches('\n');
    let mut ticks = "```".to_string();
    while content.contains(&ticks) {
        ticks.push('`');
    }
    format!("{ticks}{lang}\n{content}\n{ticks}")
}

/// Makes sure what follows starts a new block.
fn blank_before(out: &mut String) {
    if out.is_empty() {
        return;
    }
    while out.ends_with(' ') {
        out.pop();
    }
    if out.ends_with("\n\n") {
        return;
    }
    if out.ends_with('\n') {
        out.push('\n');
    } else {
        out.push_str("\n\n");
    }
}

/// No runs of blank lines, no spaces at line ends.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blanks = 0;
    let mut in_fence = false;
    for line in s.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        let line = if in_fence { line } else { line.trim_end() };
        if line.is_empty() && !in_fence {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    let out = out.trim();
    if out.is_empty() {
        String::new()
    } else {
        format!("{out}\n")
    }
}

#[cfg(test)]
mod tests;
