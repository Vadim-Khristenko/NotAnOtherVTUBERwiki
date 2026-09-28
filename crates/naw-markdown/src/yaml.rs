//! The wiki's YAML: a strict, small subset for template fields.
//!
//! One flat mapping, `key: value` a line. A value is plain text, text in
//! double or single quotes, a `|` block (lines kept) or `>` block (lines
//! joined), or a list of `- item` lines, which reads as the items joined by
//! commas. `#` starts a comment. Anchors, aliases, tags, inline `{}` and `[]`
//! collections and nested mappings are refused with the line they are on:
//! each of them either hides where a value came from or has no place in a
//! field, and a refusal is easier to fix than a surprise.

/// Why a block could not be read, and on which line (from 1). `code` names
/// the reason for a translation (`template.yaml_<code>`); `reason` is it in
/// English, for when there is none.
#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub code: &'static str,
    pub reason: &'static str,
}

/// Every error code, for whoever translates them.
pub const CODES: &[&str] = &[
    "one_block",
    "nested",
    "expected_field",
    "field_name",
    "space_after_colon",
    "duplicate",
    "unclosed_quote",
    "unknown_escape",
    "unsupported",
    "quote_value",
    "after_quote",
    "space_after_dash",
    "list_plain",
];

/// A code's reason in English.
pub fn reason(code: &str) -> &'static str {
    match code {
        "one_block" => "one block holds one set of fields",
        "nested" => "nested fields are not supported: keep one level",
        "expected_field" => "expected `field: value`",
        "field_name" => "a field name is letters, digits, `_` and `-`",
        "space_after_colon" => "put a space after the colon",
        "duplicate" => "this field is already set above",
        "unclosed_quote" => "the quote is not closed",
        "unknown_escape" => "unknown escape after a backslash",
        "unsupported" => {
            "anchors, aliases, tags and inline lists are not supported: put the text in quotes"
        }
        "quote_value" => "a value with `: ` needs quotes",
        "after_quote" => "only a comment may follow the closing quote",
        "space_after_dash" => "put a space after `-`",
        "list_plain" => "a list holds plain items, one level deep",
        _ => "this line could not be read",
    }
}

fn fail(line: usize, code: &'static str) -> Error {
    Error {
        line: line + 1,
        code,
        reason: reason(code),
    }
}

/// The fields in order, each key once.
pub fn parse(text: &str) -> Result<Vec<(String, String)>, Error> {
    // A byte order mark from an editor that saves one is not part of a key.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
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
            return Err(fail(i, "one_block"));
        }
        if line.starts_with([' ', '\t']) {
            return Err(fail(i, "nested"));
        }
        // A Japanese or Chinese keyboard types the full-width colon, and those
        // languages put no space after it.
        let Some((colon, mark)) = line.char_indices().find(|(_, c)| matches!(c, ':' | '：'))
        else {
            return Err(fail(i, "expected_field"));
        };
        let key = line[..colon].trim();
        let plain_key = !key.is_empty()
            && key.len() <= 64
            && key
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
        if !plain_key {
            return Err(fail(i, "field_name"));
        }
        let after = &line[colon + mark.len_utf8()..];
        if mark == ':' && !after.is_empty() && !after.starts_with(char::is_whitespace) {
            return Err(fail(i, "space_after_colon"));
        }
        if out.iter().any(|(k, _)| k == key) {
            return Err(fail(i, "duplicate"));
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
                None => return Err(fail(line, "unclosed_quote")),
                Some('"') => break,
                Some('\\') => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    _ => return Err(fail(line, "unknown_escape")),
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
                None => return Err(fail(line, "unclosed_quote")),
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
        return Err(fail(line, "unsupported"));
    }
    let plain = match value.find(" #") {
        Some(at) => &value[..at],
        None => value,
    }
    .trim_end();
    if plain.contains(": ") {
        return Err(fail(line, "quote_value"));
    }
    Ok(plain.to_string())
}

/// Only a comment may follow a closing quote.
fn trailing(rest: &str, line: usize) -> Result<(), Error> {
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(fail(line, "after_quote"))
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
            return Err(fail(i, "nested"));
        };
        if !item.is_empty() && !item.starts_with(' ') {
            return Err(fail(i, "space_after_dash"));
        }
        let item = item.trim();
        if item.starts_with('-') || (item.contains(": ") && !item.starts_with(['"', '\''])) {
            return Err(fail(i, "list_plain"));
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
    fn any_language_in_keys_and_values() {
        let fields =
            map("имя: Филиан\nдебют: 2021\n名前: フィリアン\ncafé: crème brûlée\nfans: Снэкерсы");
        assert_eq!(fields[0], ("имя".into(), "Филиан".into()));
        assert_eq!(fields[1], ("дебют".into(), "2021".into()));
        assert_eq!(fields[2], ("名前".into(), "フィリアン".into()));
        assert_eq!(fields[3], ("café".into(), "crème brûlée".into()));
        assert_eq!(fields[4].1, "Снэкерсы");
    }

    #[test]
    fn keyboards_of_other_languages_are_forgiven() {
        // the full-width colon a Japanese or Chinese keyboard types
        let fields = map("名前：フィリアン\nname： Fil");
        assert_eq!(fields[0], ("名前".into(), "フィリアン".into()));
        assert_eq!(fields[1], ("name".into(), "Fil".into()));
        // a no-break space after the colon, Windows line ends, a byte order mark
        let fields = map("\u{feff}name:\u{a0}Fil\r\nbio: |\r\n  one\r\n  two\r\n");
        assert_eq!(fields[0], ("name".into(), "Fil".into()));
        assert_eq!(fields[1], ("bio".into(), "one\ntwo".into()));
        // a colon inside a Russian or Japanese value is text
        let fields = map("quote: \"Он сказал: привет\"\ntime: 12:30");
        assert_eq!(fields[0].1, "Он сказал: привет");
        assert_eq!(fields[1].1, "12:30");
    }

    #[test]
    fn every_error_has_a_code() {
        for (text, code) in [
            ("a: &x y", "unsupported"),
            ("a:\n  b: c", "nested"),
            ("a: 1\na: 2", "duplicate"),
            ("a: \"open", "unclosed_quote"),
            ("a: x: y", "quote_value"),
            ("just text", "expected_field"),
            ("a:b", "space_after_colon"),
        ] {
            assert_eq!(parse(text).expect_err(text).code, code, "{text}");
        }
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
