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

use std::collections::{BTreeSet, HashMap};

/// Templates inside templates, at most this deep.
pub const DEPTH_MAX: usize = 16;

/// Template and parser function calls in one expansion.
pub const CALLS_MAX: usize = 2000;

/// Longest expanded text. A template used a thousand times in a loop of
/// copies would otherwise multiply a small page into gigabytes.
pub const OUTPUT_MAX: usize = 12 * 1024 * 1024;

/// What a run takes from the page: the text that goes where a call fails
/// (`{name}` is the template's name), and the page's language for
/// `{{PAGELANGUAGE}}`.
#[derive(Clone, Debug)]
pub struct Notes {
    pub missing: String,
    pub looped: String,
    pub limit: String,
    pub language: String,
}

impl Default for Notes {
    fn default() -> Self {
        Self {
            missing: "[Template:{name}](/template:{name})".into(),
            looped: "**Template loop: {name}**".into(),
            limit: "**Template limit reached**".into(),
            language: "en".into(),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    /// Templates found and used, by slug.
    pub used: BTreeSet<String>,
    /// Templates called but not passed in, by slug.
    pub missing: BTreeSet<String>,
}

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
    let mut run = Run {
        templates,
        notes,
        calls: 0,
        used: BTreeSet::new(),
        missing: BTreeSet::new(),
        stack: Vec::new(),
    };
    let text = run.text(source, 0);
    Expansion {
        text,
        used: run.used,
        missing: run.missing,
    }
}

/// A template's source as its own page shows it: the documentation in, the
/// parts meant for other pages out, and each parameter at its default.
pub fn template_view(source: &str) -> String {
    let shown = unwrap_tag(&drop_tag(source, "includeonly"), "noinclude");
    substitute(&shown, &HashMap::new())
}

/// A template's source as another page receives it.
fn for_inclusion(source: &str) -> String {
    let body = unwrap_tag(&drop_tag(source, "noinclude"), "includeonly");
    body.trim_end_matches(['\n', '\r']).to_string()
}

struct Run<'a> {
    templates: &'a HashMap<String, String>,
    notes: &'a Notes,
    calls: usize,
    used: BTreeSet<String>,
    missing: BTreeSet<String>,
    stack: Vec<String>,
}

impl Run<'_> {
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
            if out.len() > OUTPUT_MAX {
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
        if self.calls > CALLS_MAX || depth >= DEPTH_MAX {
            return self.notes.limit.clone();
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
        let Some(slug) = template_slug(head) else {
            return format!("{{{{{inner}}}}}");
        };
        if self.stack.contains(&slug) {
            return self.note(&self.notes.looped, &slug);
        }
        let Some(source) = self.templates.get(&slug) else {
            self.missing.insert(slug.clone());
            return self.note(&self.notes.missing, &slug);
        };
        self.used.insert(slug.clone());

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
        let body = substitute(&for_inclusion(source), &args);
        self.stack.push(slug);
        let expanded = self.text(&body, depth + 1);
        self.stack.pop();
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
        assert!(out.text.len() <= OUTPUT_MAX + 1024);
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
