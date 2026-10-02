//! Template transclusion, before Markdown runs.
//!
//! `{{Name | key=value | positional}}` is replaced by the body of the page
//! `Template:name`, with `{{{key}}}` and `{{{key|default}}}` in it filled from
//! the call. `{{#if: test | then | else}}`, `{{#ifeq: a | b | then | else}}`
//! and `{{#switch: value | case = text | #default = text}}` choose between
//! texts, `{{PAGELANGUAGE}}` is the page's language code, so a template can
//! label its fields in the reader's language, and `{{!}}` is a literal `|` for
//! a table row inside an argument.
//!
//! In a template's source, `<noinclude>...</noinclude>` shows only on the
//! template's own page (documentation) and `<includeonly>...</includeonly>`
//! only where it is used. Code spans and fenced code are left alone, and
//! `\{{` is a literal brace pair.
//!
//! Pure: the caller loads the templates. [`expand`] reports the names it
//! could not find, so the caller loads those and runs it again.

use crate::yaml;
use std::collections::{BTreeSet, HashMap};

/// How much one expansion may do, from the wiki's limits (`template_depth`,
/// `template_calls`, `template_output_bytes`). The output cap matters most:
/// a template used a thousand times in a loop of copies would otherwise
/// multiply a small page into gigabytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Templates inside templates, at most this deep.
    pub depth: usize,
    /// Template and parser function calls in one expansion.
    pub calls: usize,
    /// Longest expanded text, in bytes.
    pub output: usize,
    /// Different templates one page may pull in.
    pub templates: usize,
}

impl Budget {
    pub fn of(limits: &naw_core::limits::Limits) -> Self {
        Self {
            depth: limits.template_depth,
            calls: limits.template_calls,
            output: limits.template_output_bytes,
            templates: limits.templates_per_page,
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::of(&naw_core::limits::Limits::default())
    }
}

/// What a run takes from the page: the text that goes where a call fails
/// (`{name}` is the template's name), and the page's language for
/// `{{PAGELANGUAGE}}`.
#[derive(Clone, Debug)]
pub struct Notes {
    pub missing: String,
    pub looped: String,
    pub limit: String,
    pub language: String,
    /// YAML fields that could not be read: `{line}` and `{reason}`.
    pub bad_data: String,
    /// Fields the template's `<params>` do not list: `{names}`.
    pub unknown_fields: String,
    /// Required fields left out or empty: `{names}`.
    pub missing_fields: String,
    /// YAML error reasons in the reader's language, by code; English otherwise.
    pub yaml_reasons: HashMap<String, String>,
    /// How much the run may do.
    pub budget: Budget,
}

impl Default for Notes {
    fn default() -> Self {
        Self {
            budget: Budget::default(),
            missing: "[Template:{name}](/template:{name})".into(),
            looped: "**Template loop: {name}**".into(),
            limit: "**Template limit reached**".into(),
            language: "en".into(),
            bad_data: "**YAML, line {line}: {reason}**".into(),
            unknown_fields: "*Fields this template does not have: {names}*".into(),
            missing_fields: "*Required fields left empty: {names}*".into(),
            yaml_reasons: HashMap::new(),
        }
    }
}

/// One field a template declares in its `<params>` block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    /// `text`, `image`, `date`, `number`, `link` or `list`.
    pub kind: String,
    pub required: bool,
}

/// The kinds a `<params>` line may name; anything else reads as `text`.
pub const PARAM_KINDS: &[&str] = &["text", "image", "date", "number", "link", "list"];

/// The fields a template declares: a `<params>` block with one
/// `name: kind` or `name: kind required` a line. Empty when there is none.
pub fn params_of(source: &str) -> Vec<Param> {
    let mut out: Vec<Param> = Vec::new();
    for (key, value) in block_lines(source, "params") {
        let mut words = value.split_whitespace();
        let kind = words
            .next()
            .filter(|k| PARAM_KINDS.contains(k))
            .unwrap_or("text");
        let required = words.any(|w| w == "required");
        if !out.iter().any(|p| p.name == key) {
            out.push(Param {
                name: key,
                kind: kind.to_string(),
                required,
            });
        }
    }
    out
}

/// `key: value` or `key = value` lines of every `<tag>` block, in order.
fn block_lines(source: &str, tag: &str) -> Vec<(String, String)> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let lower = source.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(&open) {
        let start = from + at + open.len();
        let end = lower[start..]
            .find(&close)
            .map_or(source.len(), |e| start + e);
        for line in source[start..end].lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let split = match (line.find(':'), line.find('=')) {
                (Some(c), Some(e)) => Some(c.min(e)),
                (c, e) => c.or(e),
            };
            if let Some(at) = split {
                let key = line[..at].trim();
                let plain = !key.is_empty()
                    && key.len() <= 64
                    && key
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
                if plain {
                    out.push((key.to_string(), line[at + 1..].trim().to_string()));
                }
            }
        }
        from = end;
    }
    out
}

/// `Name` + a fenced ```yaml block and nothing else: the call and its data.
fn data_call(inner: &str) -> Option<(&str, &str)> {
    let (name, rest) = inner.split_once('\n')?;
    let name = name.trim();
    if name.is_empty() || name.contains('|') || name.starts_with('#') {
        return None;
    }
    let rest = rest.trim();
    let first_line = rest.lines().next()?.trim();
    if !matches!(first_line, "```yaml" | "```yml") {
        return None;
    }
    let body = rest[rest.find('\n')? + 1..].trim_end();
    let body = body.strip_suffix("```")?;
    Some((name, body))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    /// Templates found and used, by slug.
    pub used: BTreeSet<String>,
    /// Templates called but not passed in, by slug.
    pub missing: BTreeSet<String>,
    /// Templates that ended up calling themselves, by slug.
    pub looped: BTreeSet<String>,
    /// Names that are not slugs and are not among the aliases, as
    /// [`alias_key`]s: the caller looks them up by title and runs again.
    pub unresolved: BTreeSet<String>,
}

/// Field labels in the reader's language, per template slug: what a
/// translation of a template supplies instead of code of its own.
pub type Labels = HashMap<String, HashMap<String, String>>;

/// The slug a template name stands for: `Infobox VTuber`, `template:infobox_vtuber`
/// and `Шаблон:Infobox VTuber` are all `infobox-vtuber`. `None` for a name no
/// page could have.
pub fn template_slug(name: &str) -> Option<String> {
    let name = name.trim();
    let bare = ["template:", "шаблон:"]
        .iter()
        .find_map(|prefix| {
            name.get(..prefix.len())
                .filter(|head| head.to_lowercase() == *prefix)
                .map(|_| &name[prefix.len()..])
        })
        .unwrap_or(name);
    let mut slug = String::with_capacity(bare.len());
    for c in bare.trim().chars() {
        let c = match c {
            ' ' | '_' | '-' => '-',
            c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
            _ => return None,
        };
        if c == '-' && (slug.is_empty() || slug.ends_with('-')) {
            continue;
        }
        slug.push(c);
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    (!slug.is_empty() && slug.len() <= 100).then_some(slug)
}

/// Expands every call in `source` with `templates`, keyed by slug.
pub fn expand(source: &str, templates: &HashMap<String, String>, notes: &Notes) -> Expansion {
    expand_labeled(source, templates, &Labels::new(), notes)
}

/// [`expand`], with `{{#label:key|default}}` read from `labels`.
pub fn expand_labeled(
    source: &str,
    templates: &HashMap<String, String>,
    labels: &Labels,
    notes: &Notes,
) -> Expansion {
    expand_full(source, templates, labels, &HashMap::new(), notes)
}

/// [`expand_labeled`], with `aliases` naming templates by a title that is not
/// a slug: `{{Карточка VTuber}}` for `infobox-vtuber`. Keys are
/// [`alias_key`]s; a name found in none is reported in `unresolved`.
pub fn expand_full(
    source: &str,
    templates: &HashMap<String, String>,
    labels: &Labels,
    aliases: &HashMap<String, String>,
    notes: &Notes,
) -> Expansion {
    let mut run = Run {
        templates,
        labels,
        aliases,
        notes,
        calls: 0,
        used: BTreeSet::new(),
        missing: BTreeSet::new(),
        looped: BTreeSet::new(),
        unresolved: BTreeSet::new(),
        stack: Vec::new(),
    };
    let text = run.text(source, 0);
    Expansion {
        text,
        used: run.used,
        missing: run.missing,
        looped: run.looped,
        unresolved: run.unresolved,
    }
}

/// How a template name that is not a slug is looked up: without a
/// `Template:` or `Шаблон:` prefix, lowercase, `_` and runs of spaces as one
/// space. `None` for what could not be a name (braces, bars, line breaks).
pub fn alias_key(name: &str) -> Option<String> {
    let lower = name.trim().to_lowercase();
    let bare = ["template:", "шаблон:"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .unwrap_or(&lower);
    let key = bare
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let plain = !key.is_empty()
        && key.chars().count() <= 200
        && !key.contains(['{', '}', '|', '[', ']', '#', '<', '>', '\n']);
    plain.then_some(key)
}

/// A template's source as its own page shows it: the documentation in, the
/// parts meant for other pages out, and each parameter at its default.
pub fn template_view(source: &str) -> String {
    let shown = unwrap_tag(
        &drop_tag(
            &drop_tag(&drop_tag(source, "includeonly"), "labels"),
            "params",
        ),
        "noinclude",
    );
    substitute(&shown, &HashMap::new())
}

/// A template's source as another page receives it.
fn for_inclusion(source: &str) -> String {
    let body = unwrap_tag(
        &drop_tag(
            &drop_tag(&drop_tag(source, "noinclude"), "labels"),
            "params",
        ),
        "includeonly",
    );
    // Line breaks around the blocks are layout of the source, not of the call:
    // `<params>` on its own lines must not open a paragraph in every page.
    body.trim_matches(['\n', '\r']).to_string()
}

/// The `<labels>` block of a template version: one `key = text` a line;
/// blank lines and lines starting with `#` are skipped.
pub fn labels_of(source: &str) -> HashMap<String, String> {
    block_lines(source, "labels").into_iter().collect()
}

/// Whether a template version carries code of its own: anything but its
/// documentation, its labels and comments. A translation must not, so every
/// language runs the same template.
pub fn has_code(source: &str) -> bool {
    let rest = drop_tag(
        &drop_tag(&drop_tag(source, "noinclude"), "labels"),
        "params",
    );
    let mut rest = rest.as_str();
    let mut left = String::new();
    while let Some(at) = rest.find("<!--") {
        left.push_str(&rest[..at]);
        rest = rest[at..].find("-->").map_or("", |e| &rest[at + e + 3..]);
    }
    left.push_str(rest);
    !left.trim().is_empty()
}

struct Run<'a> {
    templates: &'a HashMap<String, String>,
    labels: &'a Labels,
    aliases: &'a HashMap<String, String>,
    notes: &'a Notes,
    calls: usize,
    used: BTreeSet<String>,
    missing: BTreeSet<String>,
    looped: BTreeSet<String>,
    unresolved: BTreeSet<String>,
    stack: Vec<String>,
}

impl Run<'_> {
    /// The slug a call names: the name itself when it spells one, else a
    /// template whose title it is in some language. `None` when neither;
    /// such a name is kept for the next round to look up.
    fn resolve(&mut self, name: &str) -> Option<String> {
        let slug = template_slug(name);
        if let Some(slug) = &slug
            && self.templates.contains_key(slug)
        {
            return Some(slug.clone());
        }
        if let Some(key) = alias_key(name) {
            if let Some(slug) = self.aliases.get(&key) {
                return Some(slug.clone());
            }
            if slug.is_none() {
                self.unresolved.insert(key);
            }
        }
        slug
    }

    fn note(&self, template: &str, name: &str) -> String {
        template.replace("{name}", name)
    }

    /// Expands the calls in `text`, copying code and escaped braces as they are.
    fn text(&mut self, text: &str, depth: usize) -> String {
        if !text.contains("{{") {
            return text.to_string();
        }
        let bytes = text.as_bytes();
        let braces = Braces::of(text);
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        let mut line_start = true;
        while i < bytes.len() {
            if out.len() > self.notes.budget.output {
                out.push_str(&self.notes.limit);
                break;
            }
            if line_start && let Some(end) = fenced_code_end(text, i) {
                out.push_str(&text[i..end]);
                i = end;
                line_start = true;
                continue;
            }
            line_start = false;
            match bytes[i] {
                b'\n' => {
                    out.push('\n');
                    i += 1;
                    line_start = true;
                }
                b'\\' if text[i + 1..].starts_with('{') => {
                    // `\{{` stays for Markdown, which prints the brace.
                    let run = 1 + text[i + 1..].bytes().take_while(|&b| b == b'{').count();
                    out.push_str(&text[i..i + run]);
                    i += run;
                }
                b'`' => {
                    let end = code_span_end(text, i);
                    out.push_str(&text[i..end]);
                    i = end;
                }
                b'{' if text[i..].starts_with("{{") => match braces.end(i) {
                    Some(end) if !text[i..].starts_with("{{{") => {
                        let inner = &text[i + 2..end - 2];
                        let expanded = self.call(inner, depth);
                        out.push_str(&expanded);
                        i = end;
                    }
                    // A parameter outside a template, or unclosed: text.
                    Some(end) => {
                        out.push_str(&text[i..end]);
                        i = end;
                    }
                    None => {
                        out.push_str("{{");
                        i += 2;
                    }
                },
                _ => {
                    let c = text[i..].chars().next().unwrap_or('\0');
                    out.push(c);
                    i += c.len_utf8().max(1);
                }
            }
        }
        out
    }

    fn call(&mut self, inner: &str, depth: usize) -> String {
        self.calls += 1;
        if self.calls > self.notes.budget.calls || depth >= self.notes.budget.depth {
            return self.notes.limit.clone();
        }
        // `{{Name` + a ```yaml block + `}}`: the fields as YAML.
        if let Some((name, yaml)) = data_call(inner) {
            let Some(slug) = self.resolve(name) else {
                return format!("{{{{{inner}}}}}");
            };
            let fields = match yaml::parse(yaml) {
                Ok(fields) => fields,
                Err(err) => {
                    return self
                        .notes
                        .bad_data
                        .replace("{line}", &err.line.to_string())
                        .replace(
                            "{reason}",
                            self.notes
                                .yaml_reasons
                                .get(err.code)
                                .map_or(err.reason, String::as_str),
                        );
                }
            };
            let args = fields
                .into_iter()
                .map(|(key, value)| (key, self.text(&value, depth + 1).trim().to_string()))
                .collect();
            return self.include(slug, args, depth);
        }
        let parts = split_top(inner, '|');
        let head = parts[0].trim();
        if parts.len() == 1 {
            match head {
                "!" => return "|".into(),
                "PAGELANGUAGE" => return self.notes.language.clone(),
                _ => {}
            }
        }
        if let Some(function) = head.strip_prefix('#') {
            return self.function(function, &parts[1..], depth);
        }
        let Some(slug) = self.resolve(head) else {
            return format!("{{{{{inner}}}}}");
        };
        if !self.templates.contains_key(&slug) || self.stack.contains(&slug) {
            return self.include(slug, HashMap::new(), depth);
        }
        let mut args: HashMap<String, String> = HashMap::new();
        let mut position = 0;
        for part in &parts[1..] {
            let value = self.text(part, depth + 1);
            match split_named(&value) {
                Some((key, value)) => {
                    args.insert(key, value);
                }
                None => {
                    position += 1;
                    args.insert(position.to_string(), value.trim().to_string());
                }
            }
        }
        self.include(slug, args, depth)
    }

    /// The template at `slug` with `args`, or the note for why it cannot be:
    /// missing, or already being expanded. With a `<params>` schema, a call
    /// that brings fields the template does not know, or leaves out required
    /// ones, gets a note after the template saying which.
    fn include(&mut self, slug: String, args: HashMap<String, String>, depth: usize) -> String {
        if self.stack.contains(&slug) {
            self.looped.insert(slug.clone());
            return self.note(&self.notes.looped, &slug);
        }
        let Some(source) = self.templates.get(&slug) else {
            self.missing.insert(slug.clone());
            return self.note(&self.notes.missing, &slug);
        };
        self.used.insert(slug.clone());
        let params = params_of(source);
        let body = substitute(&for_inclusion(source), &args);
        self.stack.push(slug);
        let mut expanded = self.text(&body, depth + 1);
        self.stack.pop();
        if !params.is_empty() {
            let known = |key: &str| params.iter().any(|p| p.name == key);
            let mut unknown: Vec<&str> = args
                .keys()
                .map(String::as_str)
                .filter(|key| !known(key) && key.parse::<usize>().is_err())
                .collect();
            unknown.sort_unstable();
            let missing: Vec<&str> = params
                .iter()
                .filter(|p| p.required && args.get(&p.name).is_none_or(|v| v.trim().is_empty()))
                .map(|p| p.name.as_str())
                .collect();
            if !unknown.is_empty() {
                expanded.push_str("\n\n");
                expanded.push_str(
                    &self
                        .notes
                        .unknown_fields
                        .replace("{names}", &unknown.join(", ")),
                );
            }
            if !missing.is_empty() {
                expanded.push_str("\n\n");
                expanded.push_str(
                    &self
                        .notes
                        .missing_fields
                        .replace("{names}", &missing.join(", ")),
                );
            }
        }
        expanded
    }

    /// `#if` and `#ifeq`; the first argument follows the colon in the head.
    fn function(&mut self, head: &str, rest: &[&str], depth: usize) -> String {
        let Some((name, first)) = head.split_once(':') else {
            return String::new();
        };
        let arg = |run: &mut Self, i: usize| -> String {
            rest.get(i)
                .map(|part| run.text(part, depth + 1).trim().to_string())
                .unwrap_or_default()
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "if" => {
                let test = self.text(first, depth + 1);
                if test.trim().is_empty() {
                    arg(self, 1)
                } else {
                    arg(self, 0)
                }
            }
            "ifeq" => {
                let left = self.text(first, depth + 1).trim().to_string();
                let right = arg(self, 0);
                if left == right {
                    arg(self, 1)
                } else {
                    arg(self, 2)
                }
            }
            // `{{#switch: value | a = one | b | c = shared | #default = other}}`:
            // a case without `=` falls through to the next one that has it, and a
            // last bare case is the default.
            "switch" => {
                let value = self.text(first, depth + 1).trim().to_string();
                let mut matched = false;
                let mut default = None;
                for (i, part) in rest.iter().enumerate() {
                    match part.split_once('=') {
                        Some((case, result)) => {
                            let case = self.text(case, depth + 1).trim().to_string();
                            if matched || case == value {
                                return self.text(result, depth + 1).trim().to_string();
                            }
                            if case == "#default" {
                                default = Some(result);
                            }
                        }
                        None if i + 1 == rest.len() => default = Some(part),
                        None => {
                            if self.text(part, depth + 1).trim() == value {
                                matched = true;
                            }
                        }
                    }
                }
                default
                    .map(|d| self.text(d, depth + 1).trim().to_string())
                    .unwrap_or_default()
            }
            // `{{#label: debut | Debut}}`: the field's label from the template's
            // translation in the page language, else the text after the bar.
            "label" => {
                let key = self.text(first, depth + 1).trim().to_string();
                let translated = self
                    .stack
                    .last()
                    .and_then(|slug| self.labels.get(slug))
                    .and_then(|labels| labels.get(&key))
                    .cloned();
                match translated {
                    Some(text) => self.text(&text, depth + 1).trim().to_string(),
                    None => arg(self, 0),
                }
            }
            _ => String::new(),
        }
    }
}

/// `key = value` with a plain key, trimmed on both sides.
fn split_named(part: &str) -> Option<(String, String)> {
    let at = part.find('=')?;
    let key = part[..at].trim();
    let plain = !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ' ');
    plain.then(|| (key.to_string(), part[at + 1..].trim().to_string()))
}

/// Splits on `sep` outside nested braces and `[[links|with pipes]]`.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut braces, mut links) = (0usize, 0usize);
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => braces += 1,
            b'}' => braces = braces.saturating_sub(1),
            b'[' if bytes.get(i + 1) == Some(&b'[') => {
                links += 1;
                i += 1;
            }
            b']' if bytes.get(i + 1) == Some(&b']') => {
                links = links.saturating_sub(1);
                i += 1;
            }
            b if b == sep as u8 && braces == 0 && links == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&text[start..]);
    parts
}

/// Every brace pair of a text, matched in one pass, so finding where a call
/// closes costs a lookup rather than a scan. Single braces are counted, so
/// `{{{x}}}` and nested calls balance.
struct Braces {
    /// (opening, closing) byte offsets, sorted by the opening one.
    pairs: Vec<(usize, usize)>,
}

impl Braces {
    fn of(text: &str) -> Self {
        let mut open = Vec::new();
        let mut pairs = Vec::new();
        for (i, b) in text.bytes().enumerate() {
            match b {
                b'{' => open.push(i),
                b'}' => {
                    if let Some(start) = open.pop() {
                        pairs.push((start, i));
                    }
                }
                _ => {}
            }
        }
        pairs.sort_unstable();
        Self { pairs }
    }

    /// Just past the brace that closes the one at `start`.
    fn end(&self, start: usize) -> Option<usize> {
        self.pairs
            .binary_search_by_key(&start, |pair| pair.0)
            .ok()
            .map(|k| self.pairs[k].1 + 1)
    }
}

/// Fills `{{{name}}}` and `{{{name|default}}}` from `args`. An unknown name
/// with no default stays as written, so a mistake shows on the page.
fn substitute(body: &str, args: &HashMap<String, String>) -> String {
    if !body.contains("{{{") {
        return body.to_string();
    }
    let braces = Braces::of(body);
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while let Some(at) = body[i..].find("{{{") {
        let start = i + at;
        out.push_str(&body[i..start]);
        let Some(end) = braces
            .end(start)
            .filter(|&end| body[..end].ends_with("}}}"))
        else {
            out.push_str("{{{");
            i = start + 3;
            continue;
        };
        let inner = &body[start + 3..end - 3];
        let (name, default) = match split_top(inner, '|').as_slice() {
            [name] => (name.trim(), None),
            [name, ..] => (
                name.trim(),
                Some(&inner[inner.find('|').unwrap_or(0) + 1..]),
            ),
            [] => ("", None),
        };
        match (args.get(name), default) {
            (Some(value), _) => out.push_str(value),
            (None, Some(default)) => out.push_str(&substitute(default, args)),
            (None, None) => out.push_str(&body[start..end]),
        }
        i = end;
    }
    out.push_str(&body[i..]);
    out
}

/// The end of a fenced code block opening at `at`, when a line starts one.
fn fenced_code_end(text: &str, at: usize) -> Option<usize> {
    let line_end = text[at..].find('\n').map_or(text.len(), |n| at + n);
    let line = &text[at..line_end];
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let fence_char = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let fence_len = trimmed.chars().take_while(|c| *c == fence_char).count();
    if fence_len < 3 {
        return None;
    }
    let mut pos = line_end;
    while pos < text.len() {
        let next = pos + 1;
        let end = text[next..].find('\n').map_or(text.len(), |n| next + n);
        let candidate = text[next.min(text.len())..end].trim();
        if candidate.len() >= fence_len && candidate.chars().all(|c| c == fence_char) {
            return Some((end + 1).min(text.len()));
        }
        pos = end;
    }
    Some(text.len())
}

/// The end of the code span opening at `at`, or just past its backticks when
/// no run of the same length closes it.
fn code_span_end(text: &str, at: usize) -> usize {
    let run = text[at..].bytes().take_while(|&b| b == b'`').count();
    let fence = &text[at..at + run];
    let mut search = at + run;
    while let Some(found) = text[search..].find(fence) {
        let start = search + found;
        let len = text[start..].bytes().take_while(|&b| b == b'`').count();
        if len == run {
            return start + run;
        }
        search = start + len;
    }
    at + run
}

/// `source` without any `<tag>...</tag>` section. Tags match in any case.
fn drop_tag(source: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let lower = source.to_ascii_lowercase();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while let Some(at) = lower[i..].find(&open) {
        let start = i + at;
        out.push_str(&source[i..start]);
        match lower[start..].find(&close) {
            Some(end) => i = start + end + close.len(),
            None => {
                i = source.len();
                break;
            }
        }
    }
    out.push_str(&source[i..]);
    out
}

/// `source` with the `<tag>` and `</tag>` markers removed and their content kept.
fn unwrap_tag(source: &str, tag: &str) -> String {
    let mut out = source.to_string();
    for marker in [format!("<{tag}>"), format!("</{tag}>")] {
        let mut lower = out.to_ascii_lowercase();
        while let Some(at) = lower.find(&marker) {
            out.replace_range(at..at + marker.len(), "");
            lower = out.to_ascii_lowercase();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(templates: &[(&str, &str)]) -> HashMap<String, String> {
        templates
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn run(source: &str, templates: &[(&str, &str)]) -> Expansion {
        expand(source, &with(templates), &Notes::default())
    }

    #[test]
    fn a_label_reads_the_translation_then_its_default() {
        let card = "<includeonly>{{#label:debut|Debut}} = {{{debut|}}}</includeonly>";
        let templates = with(&[("card", card)]);
        let plain = expand("{{card|debut=2021}}", &templates, &Notes::default());
        assert_eq!(plain.text, "Debut = 2021");
        let mut labels = Labels::new();
        labels.insert(
            "card".into(),
            HashMap::from([("debut".into(), "Дебют".into())]),
        );
        let ru = expand_labeled(
            "{{card|debut=2021}}",
            &templates,
            &labels,
            &Notes::default(),
        );
        assert_eq!(ru.text, "Дебют = 2021");
        // a label belongs to its own template, not to one it happens to call
        labels.insert(
            "other".into(),
            HashMap::from([("debut".into(), "нет".into())]),
        );
        let still = expand_labeled(
            "{{card|debut=2021}}",
            &templates,
            &labels,
            &Notes::default(),
        );
        assert_eq!(still.text, "Дебют = 2021");
    }

    #[test]
    fn labels_come_from_their_block() {
        let source = "<noinclude>docs</noinclude>\n<labels>\n# a comment\ndebut = Дебют\nfans = Фанаты\n\n</labels>";
        let labels = labels_of(source);
        assert_eq!(labels.get("debut").map(String::as_str), Some("Дебют"));
        assert_eq!(labels.get("fans").map(String::as_str), Some("Фанаты"));
        assert_eq!(labels.len(), 2);
    }

    #[test]
    fn a_translation_without_code_is_told_apart() {
        assert!(!has_code(
            "<!-- title: Карточка -->\n<noinclude>Описание</noinclude>\n<labels>\na = б\n</labels>\n"
        ));
        assert!(has_code("<noinclude>Описание</noinclude>\n{{{name|}}}"));
        assert!(has_code("<includeonly>x</includeonly>"));
    }

    #[test]
    fn labels_and_docs_never_reach_the_page() {
        let templates = with(&[(
            "card",
            "<labels>\na = b\n</labels><noinclude>docs</noinclude>body",
        )]);
        assert_eq!(
            expand("{{card}}", &templates, &Notes::default()).text,
            "body"
        );
        assert_eq!(template_view("<labels>\na = b\n</labels>docs"), "docs");
    }

    #[test]
    fn fields_can_come_as_yaml() {
        let card = "<includeonly>{{{name|}}} / {{{aliases|}}} / {{{bio|}}}</includeonly>";
        let source =
            "{{Card\n```yaml\nname: Filian\naliases:\n  - Fil\n  - Cat\nbio: \"a | pipe\"\n```\n}}";
        assert_eq!(
            run(source, &[("card", card)]).text,
            "Filian / Fil, Cat / a | pipe"
        );
    }

    #[test]
    fn a_yaml_error_reads_in_the_readers_language() {
        let notes = Notes {
            bad_data: "**YAML, строка {line}: {reason}**".into(),
            yaml_reasons: HashMap::from([(
                "duplicate".to_string(),
                "это поле уже задано выше".to_string(),
            )]),
            ..Notes::default()
        };
        let source = "{{Card\n```yaml\nимя: Филиан\nимя: снова\n```\n}}";
        let text = expand(source, &with(&[("card", "x")]), &notes).text;
        assert_eq!(text, "**YAML, строка 2: это поле уже задано выше**");
    }

    #[test]
    fn a_template_is_called_by_its_name_in_any_language() {
        let templates = with(&[(
            "infobox-vtuber",
            "<includeonly>card {{{имя|}}}</includeonly>",
        )]);
        let aliases =
            HashMap::from([("карточка vtuber".to_string(), "infobox-vtuber".to_string())]);
        let run = expand_full(
            "{{Карточка VTuber | имя = Филиан}} {{Шаблон:Карточка_VTuber}}",
            &templates,
            &Labels::new(),
            &aliases,
            &Notes::default(),
        );
        assert_eq!(run.text, "card Филиан card ");
        // a name nobody knows is reported for the next round to look up
        let unknown = expand("{{Неизвестный шаблон}}", &templates, &Notes::default());
        assert!(
            unknown.unresolved.contains("неизвестный шаблон"),
            "{:?}",
            unknown.unresolved
        );
        assert_eq!(unknown.text, "{{Неизвестный шаблон}}");
    }

    #[test]
    fn broken_yaml_says_where() {
        let source = "{{Card\n```yaml\nname: Filian\nbad: &x y\n```\n}}";
        assert_eq!(
            run(source, &[("card", "x")]).text,
            "**YAML, line 2: anchors, aliases, tags and inline lists are not supported: put the text in quotes**"
        );
    }

    #[test]
    fn params_flag_unknown_and_missing_fields() {
        let card = "<params>\nname: text required\ndebut: date\n</params><includeonly>{{{name|}}}</includeonly>";
        let text = run("{{card|debut=2021|colour=red}}", &[("card", card)]).text;
        assert!(
            text.contains("Fields this template does not have: colour"),
            "{text}"
        );
        assert!(text.contains("Required fields left empty: name"), "{text}");
        let fine = run("{{card|name=Fil}}", &[("card", card)]).text;
        assert_eq!(fine, "Fil");
        assert_eq!(
            params_of(card),
            vec![
                Param {
                    name: "name".into(),
                    kind: "text".into(),
                    required: true
                },
                Param {
                    name: "debut".into(),
                    kind: "date".into(),
                    required: false
                },
            ]
        );
    }

    #[test]
    fn a_template_that_reaches_itself_is_reported() {
        let looped = run("{{a}}", &[("a", "x{{b}}"), ("b", "y{{a}}")]);
        assert!(looped.looped.contains("a"));
        assert!(run("{{a}}", &[("a", "x")]).looped.is_empty());
    }

    #[test]
    fn names_become_one_slug() {
        for name in [
            "Infobox VTuber",
            "infobox_vtuber",
            "Template:Infobox VTuber",
            "шаблон:Infobox  VTuber",
            " infobox-vtuber ",
        ] {
            assert_eq!(
                template_slug(name).as_deref(),
                Some("infobox-vtuber"),
                "{name}"
            );
        }
        assert_eq!(template_slug("Филиан"), None);
        assert_eq!(template_slug(""), None);
        assert_eq!(template_slug("a/b"), None);
    }

    #[test]
    fn named_and_positional_arguments_fill_the_body() {
        let out = run(
            "Hi {{Greet|name=Filian|loud}}!",
            &[("greet", "hello {{{name}}} ({{{1}}})")],
        );
        assert_eq!(out.text, "Hi hello Filian (loud)!");
        assert!(out.used.contains("greet"));
    }

    #[test]
    fn a_missing_argument_takes_its_default_or_stays_visible() {
        let out = run("{{T}}", &[("t", "[{{{a|none}}}] [{{{b}}}] [{{{c|}}}]")]);
        assert_eq!(out.text, "[none] [{{{b}}}] []");
    }

    #[test]
    fn if_and_ifeq_choose_a_branch() {
        let body =
            "{{#if:{{{debut|}}}|Debut {{{debut}}}|No debut}} {{#ifeq:{{{kind|}}}|vtuber|V|other}}";
        assert_eq!(
            run("{{T|debut=2021}}", &[("t", body)]).text,
            "Debut 2021 other"
        );
        assert_eq!(run("{{T|kind=vtuber}}", &[("t", body)]).text, "No debut V");
    }

    #[test]
    fn switch_picks_a_case_falls_through_and_defaults() {
        let t = [("t", "{{#switch:{{{v|}}}|a=one|b|c=shared|#default=other}}")];
        assert_eq!(run("{{T|v=a}}", &t).text, "one");
        assert_eq!(run("{{T|v=b}}", &t).text, "shared");
        assert_eq!(run("{{T|v=c}}", &t).text, "shared");
        assert_eq!(run("{{T|v=z}}", &t).text, "other");
        assert_eq!(run("{{#switch:x|a=1|fallback}}", &[]).text, "fallback");
        assert_eq!(run("{{#switch:x|a=1}}", &[]).text, "");
    }

    #[test]
    fn labels_follow_the_page_language() {
        let t = with(&[(
            "label",
            "{{#switch:{{PAGELANGUAGE}}|ru=Дебют|#default=Debut}}",
        )]);
        let ru = Notes {
            language: "ru".into(),
            ..Notes::default()
        };
        assert_eq!(expand("{{Label}}", &t, &ru).text, "Дебют");
        assert_eq!(expand("{{Label}}", &t, &Notes::default()).text, "Debut");
    }

    #[test]
    fn templates_nest_and_a_loop_is_stopped() {
        let out = run("{{A}}", &[("a", "a({{B}})"), ("b", "b")]);
        assert_eq!(out.text, "a(b)");
        let looped = run("{{A}}", &[("a", "x{{B}}"), ("b", "y{{A}}")]);
        assert_eq!(looped.text, "xy**Template loop: a**");
    }

    #[test]
    fn a_missing_template_is_reported_and_linked() {
        let out = run("{{Nope|x=1}}", &[]);
        assert!(out.missing.contains("nope"));
        assert_eq!(out.text, "[Template:nope](/template:nope)");
    }

    #[test]
    fn code_and_escapes_are_left_alone() {
        let templates = [("t", "X")];
        assert_eq!(run("`{{T}}` {{T}}", &templates).text, "`{{T}}` X");
        assert_eq!(
            run("```\n{{T}}\n```\n{{T}}", &templates).text,
            "```\n{{T}}\n```\nX"
        );
        assert_eq!(run("\\{{T}}", &templates).text, "\\{{T}}");
    }

    #[test]
    fn pipes_inside_links_and_nested_calls_do_not_split_arguments() {
        let out = run(
            "{{T|a=[[Page|label]]|b={{U|z}}}}",
            &[("t", "{{{a}}} / {{{b}}}"), ("u", "<{{{1}}}>")],
        );
        assert_eq!(out.text, "[[Page|label]] / <z>");
        assert_eq!(run("{{!}}", &[]).text, "|");
    }

    #[test]
    fn noinclude_and_includeonly_split_the_template_page_from_its_uses() {
        let source =
            "<includeonly>used {{{x|0}}}</includeonly><noinclude>Docs for {{{x|0}}}</noinclude>";
        assert_eq!(run("{{T|x=5}}", &[("t", source)]).text, "used 5");
        assert_eq!(template_view(source), "Docs for 0");
    }

    #[test]
    fn a_call_that_multiplies_itself_hits_the_limit() {
        // Each level doubles; without limits this is 2^40 copies.
        let t: Vec<(String, String)> = (0..40)
            .map(|i| {
                (
                    format!("t{i}"),
                    format!("{{{{T{}}}}}{{{{T{}}}}}", i + 1, i + 1),
                )
            })
            .collect();
        let map: HashMap<String, String> = t.into_iter().collect();
        let out = expand("{{T0}}", &map, &Notes::default());
        assert!(out.text.len() <= Budget::default().output + 1024);
        assert!(out.text.contains("Template limit reached"));
    }

    #[test]
    fn unclosed_braces_stay_text_and_stay_cheap() {
        let source = "{{".repeat(100_000);
        let started = std::time::Instant::now();
        let out = run(&source, &[]);
        assert_eq!(out.text, source);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_page_without_calls_is_returned_as_is() {
        let out = run("plain {single} text", &[]);
        assert_eq!(out.text, "plain {single} text");
        assert!(out.used.is_empty() && out.missing.is_empty());
    }
}
