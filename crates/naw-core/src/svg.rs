//! SVG sanitizing for drawings the worker returns.
//!
//! The worker is trusted to draw, not to be safe: whatever it sends back is
//! rebuilt here from an allowlist of SVG elements and attributes. Scripts,
//! event handlers, `foreignObject`, links out and any reference outside the
//! document are dropped. The result is also served with a CSP that forbids
//! scripts (see the diagram route), so this is the first of two locks.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// A sanitized drawing and its size in CSS pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Clean {
    pub svg: String,
    pub width: u32,
    pub height: u32,
}

/// Why a drawing was refused as a whole.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SvgError {
    #[error("not an SVG document")]
    NotSvg,
    #[error("the SVG does not parse: {0}")]
    Parse(String),
    #[error("the SVG nests too deep or has too many elements")]
    TooComplex,
    #[error("the SVG has no usable size")]
    NoSize,
}

const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 100_000;
/// Largest side of a drawing, in pixels.
const MAX_SIDE: f64 = 20_000.0;

/// Elements kept with their attributes and children.
const ELEMENTS: &[&str] = &[
    "svg",
    "g",
    "defs",
    "title",
    "desc",
    "style",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "text",
    "tspan",
    "textPath",
    "marker",
    "symbol",
    "use",
    "linearGradient",
    "radialGradient",
    "stop",
    "clipPath",
    "mask",
    "pattern",
    "filter",
    "feBlend",
    "feColorMatrix",
    "feComposite",
    "feDropShadow",
    "feFlood",
    "feGaussianBlur",
    "feMerge",
    "feMergeNode",
    "feMorphology",
    "feOffset",
];

/// Elements kept as a plain `<g>`: a link has no place in a picture, but a
/// mermaid node with a click is wrapped in one that carries its position,
/// so the wrapper stays a group with its transform and loses its target.
const AS_GROUP: &[&str] = &["a", "switch"];

/// Attributes kept (after the value checks), besides `aria-*`.
const ATTRIBUTES: &[&str] = &[
    "id",
    "class",
    "style",
    "transform",
    "d",
    "x",
    "y",
    "x1",
    "x2",
    "y1",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "fx",
    "fy",
    "width",
    "height",
    "viewBox",
    "points",
    "pathLength",
    "fill",
    "fill-opacity",
    "fill-rule",
    "clip-rule",
    "stroke",
    "stroke-width",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-opacity",
    "opacity",
    "color",
    "display",
    "visibility",
    "overflow",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "font-variant",
    "text-anchor",
    "text-decoration",
    "dominant-baseline",
    "alignment-baseline",
    "baseline-shift",
    "letter-spacing",
    "word-spacing",
    "writing-mode",
    "dx",
    "dy",
    "rotate",
    "textLength",
    "lengthAdjust",
    "startOffset",
    "method",
    "spacing",
    "side",
    "marker-start",
    "marker-mid",
    "marker-end",
    "markerWidth",
    "markerHeight",
    "markerUnits",
    "refX",
    "refY",
    "orient",
    "preserveAspectRatio",
    "gradientUnits",
    "gradientTransform",
    "spreadMethod",
    "offset",
    "stop-color",
    "stop-opacity",
    "clip-path",
    "clipPathUnits",
    "mask",
    "maskUnits",
    "maskContentUnits",
    "patternUnits",
    "patternContentUnits",
    "patternTransform",
    "filter",
    "filterUnits",
    "primitiveUnits",
    "stdDeviation",
    "in",
    "in2",
    "result",
    "mode",
    "operator",
    "values",
    "type",
    "radius",
    "flood-color",
    "flood-opacity",
    "vector-effect",
    "paint-order",
    "shape-rendering",
    "text-rendering",
    "pointer-events",
    "role",
    "version",
    "xml:space",
];

/// Rebuilds `input` from the allowlist. The root must be `<svg>`; its width
/// and height are set from the `viewBox`, so an `<img>` knows its size.
pub fn sanitize(input: &str) -> Result<Clean, SvgError> {
    let mut reader = Reader::from_str(input);
    reader.config_mut().check_end_names = true;
    let mut out = String::with_capacity(input.len());
    // Inside a dropped element: the depth at which it opened.
    let mut skipping: Option<usize> = None;
    // Open elements, `None` for an unwrapped one (no end tag to write).
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut elements = 0usize;
    let mut size: Option<(u32, u32)> = None;
    let mut in_style = false;
    let mut style_text = String::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|err| SvgError::Parse(err.to_string()))?;
        match event {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                elements += 1;
                if elements > MAX_ELEMENTS || stack.len() >= MAX_DEPTH {
                    return Err(SvgError::TooComplex);
                }
                let mut name = e.name().as_ref().to_string();
                let regrouped = AS_GROUP.contains(&name.as_str());
                if stack.is_empty() && name != "svg" {
                    return Err(SvgError::NotSvg);
                }
                if skipping.is_some() {
                    if !empty {
                        stack.push(None);
                    }
                    continue;
                }
                if regrouped {
                    name = "g".to_string();
                }
                if !ELEMENTS.contains(&name.as_str()) {
                    if !empty {
                        skipping = Some(stack.len());
                        stack.push(None);
                    }
                    continue;
                }
                let root = stack.is_empty();
                let mut tag = String::new();
                tag.push('<');
                tag.push_str(&name);
                let mut attrs = clean_attributes(e)?;
                if regrouped {
                    attrs.retain(|(key, _)| key != "href" && key != "xlink:href");
                }
                if root {
                    let (w, h) = root_size(&attrs).ok_or(SvgError::NoSize)?;
                    size = Some((w, h));
                    tag.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
                    tag.push_str(" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
                    tag.push_str(&format!(" width=\"{w}\" height=\"{h}\""));
                }
                for (key, value) in &attrs {
                    if root && (key == "width" || key == "height") {
                        continue;
                    }
                    tag.push(' ');
                    tag.push_str(key);
                    tag.push_str("=\"");
                    escape_into(&mut tag, value, true);
                    tag.push('"');
                }
                if empty {
                    tag.push_str("/>");
                    out.push_str(&tag);
                } else {
                    tag.push('>');
                    out.push_str(&tag);
                    if name == "style" {
                        in_style = true;
                        style_text.clear();
                    }
                    stack.push(Some(name));
                }
            }
            Event::End(_) => {
                let Some(open) = stack.pop() else {
                    return Err(SvgError::Parse("unbalanced end tag".into()));
                };
                if let Some(depth) = skipping {
                    if stack.len() == depth {
                        skipping = None;
                    }
                    continue;
                }
                if let Some(name) = open {
                    if name == "style" {
                        in_style = false;
                        if safe_css(&style_text) {
                            escape_into(&mut out, &style_text, false);
                        }
                        style_text.clear();
                    }
                    out.push_str("</");
                    out.push_str(&name);
                    out.push('>');
                }
            }
            Event::Text(ref e) => {
                if skipping.is_some() || stack.is_empty() {
                    continue;
                }
                let text: &str = e.as_ref();
                if in_style {
                    style_text.push_str(text);
                } else {
                    escape_into(&mut out, text, false);
                }
            }
            Event::CData(ref e) => {
                if skipping.is_some() || stack.is_empty() {
                    continue;
                }
                let text: &str = e.as_ref();
                if in_style {
                    style_text.push_str(text);
                } else {
                    escape_into(&mut out, text, false);
                }
            }
            Event::GeneralRef(ref e) => {
                if skipping.is_some() || stack.is_empty() {
                    continue;
                }
                let resolved = match e.resolve_char_ref() {
                    Ok(Some(c)) => Some(c),
                    Ok(None) => match AsRef::<str>::as_ref(&**e) {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        "nbsp" => Some('\u{a0}'),
                        _ => None,
                    },
                    Err(_) => None,
                };
                if let Some(c) = resolved {
                    let mut buf = [0u8; 4];
                    let s = c.encode_utf8(&mut buf);
                    if in_style {
                        style_text.push_str(s);
                    } else {
                        escape_into(&mut out, s, false);
                    }
                }
            }
            // Declarations, doctypes, comments and processing instructions
            // carry nothing a drawing needs.
            Event::Decl(_) | Event::DocType(_) | Event::Comment(_) | Event::PI(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err(SvgError::Parse("unclosed elements".into()));
    }
    let (width, height) = size.ok_or(SvgError::NotSvg)?;
    Ok(Clean {
        svg: out,
        width,
        height,
    })
}

/// The element's attributes that survive, in order, with unescaped values.
fn clean_attributes(e: &BytesStart<'_>) -> Result<Vec<(String, String)>, SvgError> {
    let mut kept = Vec::new();
    for attr in e.attributes().with_checks(true) {
        let attr = attr.map_err(|err| SvgError::Parse(err.to_string()))?;
        let key = attr.key.as_ref().to_string();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| SvgError::Parse(err.to_string()))?
            .into_owned();
        let allowed = if key == "href" || key == "xlink:href" {
            // Only a reference inside this document.
            is_fragment(&value)
        } else if let Some(rest) = key.strip_prefix("aria-") {
            rest.bytes().all(|b| b.is_ascii_lowercase())
        } else {
            ATTRIBUTES.contains(&key.as_str())
        };
        if !allowed {
            continue;
        }
        if key == "style" {
            if !safe_css(&value) {
                continue;
            }
        } else if !safe_value(&value) {
            continue;
        }
        kept.push((key, value));
    }
    Ok(kept)
}

fn is_fragment(value: &str) -> bool {
    value.len() > 1
        && value.starts_with('#')
        && value[1..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

/// A plain attribute value: `url(...)` only to a fragment, no schemes.
fn safe_value(value: &str) -> bool {
    let squashed: String = value
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if squashed.contains("javascript:") || squashed.contains("data:") {
        return false;
    }
    urls_are_fragments(&squashed)
}

/// CSS from a `<style>` element or a `style` attribute: no imports, no
/// outside urls, no escapes that could spell either, no legacy script hooks.
fn safe_css(css: &str) -> bool {
    let squashed: String = css
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if squashed.contains('\\')
        || squashed.contains("@import")
        || squashed.contains("@font-face")
        || squashed.contains("expression(")
        || squashed.contains("javascript:")
        || squashed.contains("behavior:")
        || squashed.contains("-moz-binding")
        || squashed.contains("</")
        || squashed.contains("image-set(")
    {
        return false;
    }
    urls_are_fragments(&squashed)
}

/// Every `url(` in a squashed, lowercased value points into this document.
fn urls_are_fragments(squashed: &str) -> bool {
    let mut rest = squashed;
    while let Some(at) = rest.find("url(") {
        let after = &rest[at + 4..];
        let after = after.trim_start_matches(['"', '\'']);
        if !after.starts_with('#') {
            return false;
        }
        rest = after;
    }
    true
}

/// The root's size: the `viewBox` when there is one (mermaid writes
/// `width="100%"`), else plain numeric width and height.
fn root_size(attrs: &[(String, String)]) -> Option<(u32, u32)> {
    let get = |name: &str| {
        attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let (w, h) = if let Some(view_box) = get("viewBox") {
        let parts: Vec<f64> = view_box
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        if parts.len() != 4 {
            return None;
        }
        (parts[2], parts[3])
    } else {
        let number = |s: &str| {
            s.trim_end_matches("px")
                .trim_end_matches("pt")
                .parse::<f64>()
                .ok()
        };
        (number(get("width")?)?, number(get("height")?)?)
    };
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 || w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    Some((w.ceil() as u32, h.ceil() as u32))
}

fn escape_into(out: &mut String, text: &str, attribute: bool) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(svg: &str) -> String {
        sanitize(svg).expect("sanitizes").svg
    }

    #[test]
    fn keeps_a_plain_drawing_and_sizes_it_from_the_view_box() {
        let out = sanitize(
            r#"<svg width="100%" viewBox="4 4 168.2 219" style="max-width: 168px;"><g class="node"><rect x="1" y="2" width="3" height="4" fill="url(#g)"/><text x="5">Filian &amp; Snackers</text></g></svg>"#,
        )
        .unwrap();
        assert_eq!((out.width, out.height), (169, 219));
        assert!(
            out.svg
                .starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg""#),
            "{}",
            out.svg
        );
        assert!(out.svg.contains(r#"width="169" height="219""#));
        assert!(!out.svg.contains("100%"));
        assert!(out.svg.contains("Filian &amp; Snackers"));
        assert!(out.svg.contains(r#"fill="url(#g)""#));
    }

    #[test]
    fn drops_scripts_handlers_and_foreign_objects_with_their_children() {
        let out = clean(
            r#"<svg viewBox="0 0 10 10"><script>alert(1)</script><g onclick="alert(1)" onload="x"><foreignObject><div xmlns="http://www.w3.org/1999/xhtml"><img src="x" onerror="alert(1)"/>hi</div></foreignObject><circle r="1"/></g></svg>"#,
        );
        assert!(!out.contains("script"), "{out}");
        assert!(!out.contains("alert"), "{out}");
        assert!(!out.contains("onclick"), "{out}");
        assert!(!out.contains("foreignObject"), "{out}");
        assert!(!out.contains("hi"), "{out}");
        assert!(out.contains("<circle r=\"1\"/>"), "{out}");
    }

    #[test]
    fn links_become_groups_and_outside_references_are_dropped() {
        let out = clean(
            r##"<svg viewBox="0 0 10 10"><a xlink:href="javascript:alert(1)"><path d="M0 0"/></a><use href="https://evil.example/x.svg#a"/><use xlink:href="#local"/><rect fill="url(https://evil.example/p)" style="fill:url(//x)"/><image href="data:image/png;base64,AAAA"/></svg>"##,
        );
        assert!(out.contains(r#"<g><path d="M0 0"/></g>"#), "{out}");
        assert!(!out.contains("javascript"), "{out}");
        assert!(!out.contains("evil"), "{out}");
        assert!(out.contains(r##"<use xlink:href="#local"/>"##), "{out}");
        assert!(!out.contains("image"), "{out}");
        assert!(!out.contains("//x"), "{out}");
    }

    #[test]
    fn a_clickable_mermaid_node_keeps_its_position() {
        let out = clean(
            r##"<svg viewBox="0 0 10 10"><a data-look="neo" transform="translate(88, 194.5)" xlink:href="#x"><g class="node"><text>The Wall</text></g></a></svg>"##,
        );
        assert!(
            out.contains(r#"<g transform="translate(88, 194.5)"><g class="node"><text>The Wall</text></g></g>"#),
            "{out}"
        );
    }

    #[test]
    fn a_style_element_survives_only_when_safe() {
        let ok = clean(
            r#"<svg viewBox="0 0 1 1"><style>#d1 .node rect{fill:#333;stroke:url(#grad)}</style></svg>"#,
        );
        assert!(
            ok.contains("#d1 .node rect{fill:#333;stroke:url(#grad)}"),
            "{ok}"
        );
        for bad in [
            "@import url(https://evil.example/a.css);",
            "a{background:url(https://evil.example/p.png)}",
            "a{background:url( 'https://x' )}",
            "a{b:\\75 rl(x)}",
            "@font-face{src:url(#a)}",
        ] {
            let out = clean(&format!(
                r#"<svg viewBox="0 0 1 1"><style>{bad}</style></svg>"#
            ));
            assert!(out.contains("<style></style>"), "{bad} -> {out}");
        }
    }

    #[test]
    fn style_text_cannot_close_the_element() {
        let out = clean(
            r#"<svg viewBox="0 0 1 1"><style><![CDATA[a{fill:red}</style><script>x</script>]]></style></svg>"#,
        );
        assert!(!out.contains("<script"), "{out}");
    }

    #[test]
    fn graphviz_output_with_doctype_and_comments_is_cleaned() {
        let out = clean(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x.dtd\">\n<!-- Generated by graphviz -->\n<svg width=\"62pt\" height=\"116pt\" viewBox=\"0.00 0.00 62.00 116.00\"><g id=\"graph0\"><title>%3</title></g></svg>",
        );
        assert!(
            !out.contains("DOCTYPE") && !out.contains("Generated"),
            "{out}"
        );
        assert!(out.contains(r#"width="62" height="116""#), "{out}");
    }

    #[test]
    fn refuses_what_is_not_an_svg_drawing() {
        assert_eq!(sanitize("<html><svg/></html>"), Err(SvgError::NotSvg));
        assert_eq!(sanitize("<svg><g/></svg>"), Err(SvgError::NoSize));
        assert_eq!(
            sanitize(r#"<svg viewBox="0 0 99999 10"/>"#),
            Err(SvgError::NoSize)
        );
        assert!(matches!(
            sanitize("<svg viewBox=\"0 0 1 1\"><g>"),
            Err(SvgError::Parse(_))
        ));
        let deep = format!(
            "<svg viewBox=\"0 0 1 1\">{}{}</svg>",
            "<g>".repeat(80),
            "</g>".repeat(80)
        );
        assert_eq!(sanitize(&deep), Err(SvgError::TooComplex));
    }
}
