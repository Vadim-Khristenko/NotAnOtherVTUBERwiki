//! FANDOM's portable infoboxes, as the engine's `:::infobox`.
//!
//! A portable infobox is a template written as XML:
//!
//! ```text
//! <infobox>
//!   <title source="title1"><default>{{PAGENAME}}</default></title>
//!   <image source="image1"><caption source="caption1"/></image>
//!   <group>
//!     <header>Basic Details</header>
//!     <data source="debut_date"><label>Debut Date</label></data>
//!   </group>
//! </infobox>
//! ```
//!
//! It becomes a `:::infobox {{{title1|}}}` block with a picture line, a
//! `Debut Date = {{{debut_date|}}}` row per field and a bold line per
//! header, so the articles calling it keep their calls unchanged. The
//! parameters it shows as pictures are reported, so those arguments in the
//! articles can become the engine's file addresses.

use std::collections::BTreeSet;

/// Tags a portable infobox is made of.
const TAGS: &[&str] = &[
    "infobox",
    "title",
    "image",
    "caption",
    "data",
    "label",
    "default",
    "format",
    "header",
    "group",
    "navigation",
    "panel",
    "section",
    "alt",
];

enum Item {
    Text(String),
    Element(Element),
}

struct Element {
    name: String,
    attrs: String,
    children: Vec<Item>,
}

impl Element {
    fn source(&self) -> Option<String> {
        attr(&self.attrs, "source").filter(|s| !s.is_empty())
    }

    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find_map(|item| match item {
            Item::Element(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    /// Everything inside as source text, tags of other kinds included.
    fn text(&self) -> String {
        let mut out = String::new();
        for item in &self.children {
            match item {
                Item::Text(t) => out.push_str(t),
                Item::Element(e) => out.push_str(&e.text()),
            }
        }
        out.trim().to_string()
    }
}

/// What an infobox became: the lines between `:::infobox` and `:::`, the
/// title, and the parameters shown as pictures.
pub(crate) struct Rewritten {
    pub title: String,
    pub lines: Vec<String>,
    pub images: BTreeSet<String>,
}

/// The first `<infobox>` in `text`: where it starts and ends, and what it
/// becomes.
pub(crate) fn find(text: &str) -> Option<(usize, usize, Rewritten)> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("<infobox")?;
    let after = &lower[start + "<infobox".len()..];
    if !after.starts_with(['>', ' ', '\t', '\n', '/']) {
        return None;
    }
    let (items, end) = parse(text, start)?;
    let Some(Item::Element(infobox)) = items.into_iter().next() else {
        return None;
    };
    let mut rewritten = Rewritten {
        title: String::new(),
        lines: Vec::new(),
        images: BTreeSet::new(),
    };
    emit(&infobox.children, &mut rewritten);
    Some((start, end, rewritten))
}

/// The parameters every infobox in `text` shows as pictures.
pub fn image_params(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some((_, end, rewritten)) = find(rest) {
        found.extend(rewritten.images);
        rest = &rest[end..];
    }
    found
}

/// Parses the element at `start` (which is `<name`), returning it and the
/// index past its closing tag.
fn parse(text: &str, start: usize) -> Option<(Vec<Item>, usize)> {
    let gt = text[start..].find('>')? + start;
    let head = &text[start + 1..gt];
    let self_closing = head.ends_with('/');
    let head = head.trim_end_matches('/');
    let name: String = head
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    let attrs = head[name.len()..].trim().to_string();
    if self_closing {
        return Some((
            vec![Item::Element(Element {
                name,
                attrs,
                children: Vec::new(),
            })],
            gt + 1,
        ));
    }
    let mut children = Vec::new();
    let mut i = gt + 1;
    let mut text_start = i;
    let close = format!("</{name}");
    while i < text.len() {
        let rest = &text[i..];
        if !rest.starts_with('<') {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with(&close) {
            push_text(&mut children, &text[text_start..i]);
            let end = rest.find('>').map_or(text.len(), |g| i + g + 1);
            return Some((
                vec![Item::Element(Element {
                    name,
                    attrs,
                    children,
                })],
                end,
            ));
        }
        let child_name: String = lower[1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if TAGS.contains(&child_name.as_str())
            && lower[1 + child_name.len()..].starts_with(['>', ' ', '\t', '\n', '/'])
        {
            push_text(&mut children, &text[text_start..i]);
            let (mut items, end) = parse(text, i)?;
            children.append(&mut items);
            i = end;
            text_start = i;
            continue;
        }
        i += 1;
    }
    None
}

fn push_text(children: &mut Vec<Item>, text: &str) {
    if !text.trim().is_empty() {
        children.push(Item::Text(text.to_string()));
    }
}

fn emit(items: &[Item], out: &mut Rewritten) {
    for item in items {
        match item {
            Item::Text(t) => {
                for line in t.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    out.lines.push(line.to_string());
                }
            }
            Item::Element(e) => match e.name.as_str() {
                "title" => {
                    let default = default_of(e);
                    out.title = match e.source() {
                        Some(s) => format!("{{{{{{{s}|{default}}}}}}}"),
                        None => default,
                    };
                }
                "image" => {
                    let Some(source) = e.source() else { continue };
                    out.images.insert(source.clone());
                    let caption = e.child("caption").and_then(Element::source);
                    let alt = caption
                        .as_deref()
                        .map(|c| format!("{{{{{{{c}|}}}}}}"))
                        .unwrap_or_default();
                    out.lines.push(format!(
                        "{{{{#if:{{{{{{{source}|}}}}}}|![{alt}]({{{{{{{source}}}}}}})}}}}"
                    ));
                    if let Some(c) = caption {
                        out.lines
                            .push(format!("{{{{#if:{{{{{{{c}|}}}}}}|''{{{{{{{c}}}}}}}''}}}}"));
                    }
                }
                "data" => {
                    let Some(source) = e.source() else { continue };
                    let label = e
                        .child("label")
                        .map(Element::text)
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| source.clone());
                    let default = default_of(e);
                    let value = match e.child("format").map(Element::text) {
                        Some(format) if !format.is_empty() => {
                            format!("{{{{#if:{{{{{{{source}|}}}}}}|{format}|{default}}}}}")
                        }
                        _ => format!("{{{{{{{source}|{default}}}}}}}"),
                    };
                    out.lines.push(format!("{} = {value}", row_label(&label)));
                }
                "header" => {
                    let text = e.text();
                    if !text.is_empty() {
                        out.lines.push(format!("'''{text}'''"));
                    }
                }
                "group" | "panel" | "section" => emit(&e.children, out),
                "navigation" => {
                    let text = e.text();
                    if !text.is_empty() {
                        out.lines.push(text);
                    }
                }
                _ => {}
            },
        }
    }
}

/// A field's default, leaving out `{{PAGENAME}}`, which would name the
/// template rather than the page calling it.
fn default_of(e: &Element) -> String {
    let default = e.child("default").map(Element::text).unwrap_or_default();
    if default.to_ascii_uppercase().contains("{{PAGENAME}}") {
        String::new()
    } else {
        default
    }
}

/// A label as an infobox row's key may spell it: plain words.
fn row_label(label: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in label.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '[' | ']' | '(' | ')' | '`' | '*' | '_' | '\'' | '=' | '|' | '{' | '}' if !in_tag => {}
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    out.trim_start_matches(['#', '!', '>', '-', '+'])
        .trim()
        .to_string()
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))?;
    let rest = attrs[at + name.len() + 1..].trim_start();
    let value = if let Some(r) = rest.strip_prefix('"') {
        r.split('"').next()
    } else if let Some(r) = rest.strip_prefix('\'') {
        r.split('\'').next()
    } else {
        rest.split_whitespace().next()
    };
    value.map(|v| v.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHARACTER: &str = "<infobox>\n  <title source=\"title1\">\n    <default>{{PAGENAME}}</default>\n  </title>\n\
        <image source=\"image1\">\n    <caption source=\"caption1\"/>\n  </image>\n  <group>\n    <header>Basic Details</header>\n\
        <data source=\"nick_name\">\n      <label>Nickname(s)</label>\n    </data>\n\
        <data source=\"height\"><label>Height</label><format>{{{height}}} cm</format><default>Unknown</default></data>\n\
        </group>\n</infobox><noinclude>Docs</noinclude>";

    #[test]
    fn a_portable_infobox_becomes_rows_a_picture_and_headers() {
        let (start, end, r) = find(CHARACTER).expect("an infobox");
        assert_eq!(start, 0);
        assert_eq!(&CHARACTER[end..], "<noinclude>Docs</noinclude>");
        assert_eq!(r.title, "{{{title1|}}}");
        assert_eq!(
            r.lines,
            vec![
                "{{#if:{{{image1|}}}|![{{{caption1|}}}]({{{image1}}})}}",
                "{{#if:{{{caption1|}}}|''{{{caption1}}}''}}",
                "'''Basic Details'''",
                "Nicknames = {{{nick_name|}}}",
                "Height = {{#if:{{{height|}}}|{{{height}}} cm|Unknown}}",
            ]
        );
        assert_eq!(
            image_params(CHARACTER),
            BTreeSet::from(["image1".to_string()])
        );
    }

    #[test]
    fn text_without_an_infobox_has_none() {
        assert!(find("{{Character|a=b}} <infoboxes>").is_none());
        assert!(image_params("plain").is_empty());
    }
}
