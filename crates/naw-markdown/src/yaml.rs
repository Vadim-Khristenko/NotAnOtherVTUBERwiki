//! The wiki's YAML: a strict, small subset for template fields.
//!
//! One flat mapping, `key: value` a line. A value is plain text, text in
//! double or single quotes, a `|` block (lines kept) or `>` block (lines
//! joined), or a list of `- item` lines, which reads as the items joined by
//! commas. `#` starts a comment. Anchors, aliases, tags, inline `{}` and `[]`
//! collections and nested mappings are refused with the line they are on:
//! each of them either hides where a value came from or has no place in a
//! field, and a refusal is easier to fix than a surprise.

/// Why a block could not be read, and on which line (from 1).
#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub reason: &'static str,
}

fn fail(line: usize, reason: &'static str) -> Error {
    Error {
        line: line + 1,
        reason,
    }
}

/// The fields in order, each key once.
pub fn parse(text: &str) -> Result<Vec<(String, String)>, Error> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    let mut started = false;
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            i += 1;
            continue;
        }
        if !started && line == "---" {
            started = true;
            i += 1;
            continue;
        }
        started = true;
        if line == "..." {
            break;
        }
        if line == "---" {
            return Err(fail(i, "one block holds one set of fields"));
        }
        if line.starts_with([' ', '\t']) {
            return Err(fail(i, "nested fields are not supported: keep one level"));
        }
        let Some(colon) = line.find(':') else {
            return Err(fail(i, "expected `field: value`"));
        };
        let key = line[..colon].trim();
        let plain_key = !key.is_empty()
            && key.len() <= 64
            && key
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
        if !plain_key {
            return Err(fail(i, "a field name is letters, digits, `_` and `-`"));
        }
        let after = &line[colon + 1..];
        if !after.is_empty() && !after.starts_with([' ', '\t']) {
            return Err(fail(i, "put a space after the colon"));
        }
        if out.iter().any(|(k, _)| k == key) {
            return Err(fail(i, "this field is already set above"));
        }
        let value = after.trim();
        let (parsed, next) = match value {
            "|" | "|-" | ">" | ">-" => {
                let (block, next) = block(&lines, i + 1);
                let joined = if value.starts_with('>') {
                    block
                        .split('\n')
                        .map(str::trim)
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    block
                };
                (joined.trim_end().to_string(), next)
            }
            "" => list(&lines, i + 1)?,
            _ => (scalar(value, i)?, i + 1),
        };
        out.push((key.to_string(), parsed));
        i = next;
    }
    Ok(out)
}

/// A one-line value: quoted, or plain with a trailing comment cut off.
fn scalar(value: &str, line: usize) -> Result<String, Error> {
    if let Some(rest) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        loop {
            match chars.next() {
                None => return Err(fail(line, "the quote is not closed")),
                Some('"') => break,
                Some('\\') => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    _ => return Err(fail(line, "unknown escape after `\\`")),
                },
                Some(c) => out.push(c),
            }
        }
        return trailing(chars.as_str(), line).map(|()| out);
    }
    if let Some(rest) = value.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        loop {
            match chars.next() {
                None => return Err(fail(line, "the quote is not closed")),
                Some('\'') if chars.peek() == Some(&'\'') => {
                    chars.next();
                    out.push('\'');
                }
                Some('\'') => break,
                Some(c) => out.push(c),
            }
        }
        let rest: String = chars.collect();
        return trailing(&rest, line).map(|()| out);
    }
    if value.starts_with(['&', '*', '!', '{', '[', '%', '@', '`']) {
        return Err(fail(
            line,
            "anchors, aliases, tags and inline lists are not supported: put the text in quotes",
        ));
    }
    let plain = match value.find(" #") {
        Some(at) => &value[..at],
        None => value,
    }
    .trim_end();
    if plain.contains(": ") {
        return Err(fail(line, "a value with `: ` needs quotes"));
    }
    Ok(plain.to_string())
}

/// Only a comment may follow a closing quote.
fn trailing(rest: &str, line: usize) -> Result<(), Error> {
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(fail(line, "only a comment may follow the closing quote"))
    }
}

/// The indented lines after `|` or `>`, dedented, and the line after them.
fn block(lines: &[&str], from: usize) -> (String, usize) {
    let mut end = from;
    while end < lines.len() && (lines[end].trim().is_empty() || lines[end].starts_with([' ', '\t']))
    {
        end += 1;
    }
    // trailing blank lines belong to the next field
    while end > from && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let indent = lines[from..end]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let text = lines[from..end]
        .iter()
        .map(|l| if l.len() >= indent { &l[indent..] } else { "" })
        .collect::<Vec<_>>()
        .join("\n");
    (text, end)
}

/// `- item` lines under an empty value, joined by commas; no items is empty.
fn list(lines: &[&str], from: usize) -> Result<(String, usize), Error> {
    let mut items = Vec::new();
    let mut i = from;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        if !line.starts_with([' ', '\t', '-']) {
            break;
        }
        let Some(item) = trimmed.strip_prefix('-') else {
            return Err(fail(i, "nested fields are not supported: keep one level"));
        };
        if !item.is_empty() && !item.starts_with(' ') {
            return Err(fail(i, "put a space after `-`"));
        }
        let item = item.trim();
        if item.starts_with('-') || (item.contains(": ") && !item.starts_with(['"', '\''])) {
            return Err(fail(i, "a list holds plain items, one level deep"));
        }
        items.push(scalar(item, i)?);
        i += 1;
    }
    Ok((items.join(", "), i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(text: &str) -> Vec<(String, String)> {
        parse(text).expect("parses")
    }

    #[test]
    fn plain_quoted_and_empty_values() {
        let fields = map("name: Filian\nquote: \"She said: hi\"\nsingle: 'it''s'\nempty:\n");
        assert_eq!(
            fields,
            vec![
                ("name".into(), "Filian".into()),
                ("quote".into(), "She said: hi".into()),
                ("single".into(), "it's".into()),
                ("empty".into(), String::new()),
            ]
        );
    }

    #[test]
    fn blocks_keep_or_fold_their_lines() {
        let fields = map("bio: |\n  first line\n  second line\nshort: >\n  one\n  two\nafter: x");
        assert_eq!(fields[0].1, "first line\nsecond line");
        assert_eq!(fields[1].1, "one two");
        assert_eq!(fields[2], ("after".into(), "x".into()));
    }

    #[test]
    fn a_list_reads_as_commas() {
        let fields = map("aliases:\n  - Fil\n  - \"The Cat\"\nnext: y");
        assert_eq!(fields[0].1, "Fil, The Cat");
        assert_eq!(fields[1].1, "y");
    }

    #[test]
    fn comments_and_markers_are_skipped() {
        let fields = map("---\n# a note\nname: Fil # the short one\n...\nignored: yes");
        assert_eq!(fields, vec![("name".into(), "Fil".into())]);
    }

    #[test]
    fn the_unsafe_and_the_unclear_are_refused_with_their_line() {
        for (text, line) in [
            ("a: &anchor x", 1),
            ("a: 1\nb: *alias", 2),
            ("a: !!str x", 1),
            ("a: {x: 1}", 1),
            ("a: [1, 2]", 1),
            ("a:\n  b: nested", 2),
            ("a: 1\na: 2", 2),
            ("a: \"open", 1),
            ("a: x: y", 1),
            ("just text", 1),
            ("a:b", 1),
        ] {
            let err = parse(text).expect_err(text);
            assert_eq!(err.line, line, "{text}");
        }
    }
}
