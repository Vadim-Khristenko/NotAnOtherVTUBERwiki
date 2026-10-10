//! Modules: JavaScript the server runs to make part of a page.
//!
//! A module is a page at `/module:<name>`. Its first ```js block is the code;
//! the rest of the page documents it. A page calls one of its functions on a
//! line of its own:
//!
//! ```text
//! :::Module:Stats:summary
//! ```
//!
//! optionally followed by `key = value` lines and a closing `:::`, which the
//! function receives as `args`. The function also gets `wiki` (its name,
//! language and live statistics) and `page` (the calling page's path and
//! language), and returns Markdown, which takes the call's place before the
//! page is rendered. So a module can show numbers that change, such as the
//! wiki's statistics, without a hard-coded template.
//!
//! The code runs in boa, a JavaScript engine inside the engine, with no
//! access to files, the network or the database: everything a module may
//! know comes in through its arguments. Loops, recursion and the stack are
//! capped, as are the size of the code and of what it returns, and a result
//! is kept for a minute. Only admins edit modules (see `pages::namespace_floor`).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::resolve::Ctx;

/// The prefix of a module's path: `/module:stats`.
pub(crate) const PREFIX: &str = "module:";
/// Largest module code, in bytes.
const CODE_MAX: usize = 64 * 1024;
/// Largest text one call may return, in bytes.
const OUTPUT_MAX: usize = 64 * 1024;
/// Module calls one page may make.
const CALLS_MAX: usize = 20;
/// Loop iterations one call may run.
const LOOP_MAX: u64 = 1_000_000;
/// Nested calls one call may make.
const RECURSION_MAX: usize = 256;
/// How long a result and the wiki's statistics are kept.
const KEEP: Duration = Duration::from_secs(60);
/// Results kept at most, across wikis.
const RESULTS_MAX: usize = 1000;

/// Helpers every module may use.
const PRELUDE: &str = r#"
function fmt(n, sep) {
  sep = sep === undefined ? " " : sep;
  return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, sep);
}
function plural(n, one, few, many) {
  if (many === undefined) return n === 1 ? one : few;
  var m10 = n % 10, m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
  return many;
}
"#;

/// One call found in a page.
#[derive(Debug, PartialEq)]
struct Call {
    /// Lines `[start, end)` the call takes in the source.
    start: usize,
    end: usize,
    module: String,
    function: String,
    args: Vec<(String, String)>,
}

/// `:::Module:Name:function`, any case for the word, or `None`.
fn opener(line: &str) -> Option<(String, String)> {
    let rest = line.trim();
    let rest = rest.strip_prefix(":::")?;
    let (word, rest) = rest.split_once(':')?;
    if !word.eq_ignore_ascii_case("module") {
        return None;
    }
    let (module, function) = rest.split_once(':')?;
    let module = module.trim().to_ascii_lowercase();
    let function = function.trim();
    let name_ok = !module.is_empty()
        && module.len() <= 100
        && module
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let fn_ok = !function.is_empty()
        && function.len() <= 64
        && function
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && function
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    (name_ok && fn_ok).then(|| (module, function.to_string()))
}

/// `key = value` for a call's arguments.
fn arg_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let ok = !key.is_empty()
        && key.len() <= 40
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then(|| (key.to_string(), value.trim().to_string()))
}

/// Every call in the text, outside code fences.
fn calls(text: &str) -> Vec<Call> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut found = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let run_char = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        if let Some(c) = run_char {
            let n = trimmed.chars().take_while(|x| *x == c).count();
            if n >= 3 {
                match fence {
                    None => fence = Some((c, n)),
                    Some((fc, fnn)) if fc == c && n >= fnn && trimmed.trim_end().len() == n => {
                        fence = None
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
        }
        if fence.is_some() {
            i += 1;
            continue;
        }
        if let Some((module, function)) = opener(lines[i]) {
            // Arguments: `key = value` lines (or blank ones) up to a closing `:::`.
            let mut args = Vec::new();
            let mut j = i + 1;
            let mut closed = None;
            while j < lines.len() && j - i <= 50 {
                let line = lines[j].trim();
                if line == ":::" {
                    closed = Some(j);
                    break;
                }
                if line.is_empty() {
                    j += 1;
                    continue;
                }
                match arg_line(line) {
                    Some(arg) => args.push(arg),
                    None => break,
                }
                j += 1;
            }
            let end = match closed {
                Some(j) => j + 1,
                None => {
                    args.clear();
                    i + 1
                }
            };
            found.push(Call {
                start: i,
                end,
                module,
                function,
                args,
            });
            i = end;
            continue;
        }
        i += 1;
    }
    found
}

/// The code of a module page: its first ```js or ```javascript block.
fn code_of(body: &str) -> Option<String> {
    let mut in_code = false;
    let mut fence_len = 0;
    let mut code = Vec::new();
    for line in body.split('\n') {
        let trimmed = line.trim_start();
        if !in_code {
            let n = trimmed.chars().take_while(|c| *c == '`').count();
            if n >= 3 {
                let info = trimmed[n..].trim().to_ascii_lowercase();
                if info == "js" || info == "javascript" {
                    in_code = true;
                    fence_len = n;
                }
            }
            continue;
        }
        let n = trimmed.chars().take_while(|c| *c == '`').count();
        if n >= fence_len && trimmed.trim_end().len() == n {
            return Some(code.join("\n"));
        }
        code.push(line);
    }
    None
}

/// Runs the calls in `text` and puts what each returns in its place.
pub(crate) async fn expand(
    state: &AppState,
    ctx: &Ctx,
    path: &str,
    text: String,
) -> Result<String, AppError> {
    if !text.contains(":::") || crate::pages::split_path(path).0 == "module" {
        return Ok(text);
    }
    let found = calls(&text);
    if found.is_empty() {
        return Ok(text);
    }
    let wiki = wiki_value(state, ctx).await?;
    let page = json!({ "path": path, "lang": ctx.content_locale });
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut last = 0;
    for (n, call) in found.iter().enumerate() {
        out.extend(lines[last..call.start].iter().map(|l| l.to_string()));
        let result = if n >= CALLS_MAX {
            Err(ctx.t("module.too_many_calls"))
        } else {
            run_call(state, ctx, call, &wiki, &page).await?
        };
        match result {
            Ok(markdown) => out.push(markdown),
            Err(reason) => out.push(format!(
                "\n> [!CAUTION]\n> {}\n",
                ctx.t_with(
                    "module.error",
                    &[
                        ("module", &call.module),
                        ("function", &call.function),
                        ("reason", &reason.replace('\n', " ")),
                    ],
                )
            )),
        }
        last = call.end;
    }
    out.extend(lines[last..].iter().map(|l| l.to_string()));
    Ok(out.join("\n"))
}

/// One call: from the cache, or loaded and run. The outer error is the
/// engine's; the inner one is a reason to show on the page.
async fn run_call(
    state: &AppState,
    ctx: &Ctx,
    call: &Call,
    wiki: &Value,
    page: &Value,
) -> Result<Result<String, String>, AppError> {
    let Some((revision, code)) = load(state, ctx.wiki.id, &call.module).await? else {
        return Ok(Err(
            ctx.t_with("module.missing", &[("module", &call.module)])
        ));
    };
    let Some(code) = code else {
        return Ok(Err(ctx.t("module.no_code")));
    };
    if code.len() > CODE_MAX {
        return Ok(Err(ctx.t("module.too_long")));
    }
    let args: serde_json::Map<String, Value> = call
        .args
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let key = format!(
        "{}|{revision}|{}|{}|{}|{}",
        ctx.wiki.id,
        call.function,
        serde_json::to_string(&args).unwrap_or_default(),
        page,
        wiki
    );
    if let Some(hit) = cached(&key) {
        return Ok(Ok(hit));
    }
    let function = call.function.clone();
    let input = (Value::Object(args), wiki.clone(), page.clone());
    let ran = tokio::task::spawn_blocking(move || run(&code, &function, input))
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "module task failed");
            AppError::Internal
        })?;
    if let Ok(markdown) = &ran {
        remember(key, markdown.clone());
    }
    Ok(ran)
}

/// Runs `function` from `code` with the arguments, in a fresh engine.
fn run(
    code: &str,
    function: &str,
    (args, wiki, page): (Value, Value, Value),
) -> Result<String, String> {
    use boa_engine::{Context, JsValue, Source, js_string};

    let mut context = Context::default();
    let limits = context.runtime_limits_mut();
    limits.set_loop_iteration_limit(LOOP_MAX);
    limits.set_recursion_limit(RECURSION_MAX);
    let error = |err: boa_engine::JsError| err.to_string().chars().take(300).collect::<String>();
    context.eval(Source::from_bytes(PRELUDE)).map_err(error)?;
    context.eval(Source::from_bytes(code)).map_err(error)?;
    let callee = context
        .global_object()
        .get(js_string!(function), &mut context)
        .map_err(error)?;
    let Some(callee) = callee.as_callable() else {
        return Err(format!("no function named {function}"));
    };
    let args = JsValue::from_json(&args, &mut context).map_err(error)?;
    let wiki = JsValue::from_json(&wiki, &mut context).map_err(error)?;
    let page = JsValue::from_json(&page, &mut context).map_err(error)?;
    let result = callee
        .call(&JsValue::undefined(), &[args, wiki, page], &mut context)
        .map_err(error)?;
    if result.is_undefined() || result.is_null() {
        return Ok(String::new());
    }
    let text = result
        .to_string(&mut context)
        .map_err(error)?
        .to_std_string_escaped();
    if text.len() > OUTPUT_MAX {
        return Err(format!(
            "the result is longer than {} KB",
            OUTPUT_MAX / 1024
        ));
    }
    Ok(text)
}

/// A module's current revision and its code, or `None` when there is no
/// such module. One code for every language: the wiki's own language wins.
async fn load(
    state: &AppState,
    wiki_id: Uuid,
    module: &str,
) -> Result<Option<(Uuid, Option<String>)>, AppError> {
    let row = sqlx::query!(
        r#"SELECT r.id, r.body_md FROM pages p
           JOIN revisions r ON r.id = p.current_revision_id
           JOIN wikis w ON w.id = p.wiki_id
           WHERE p.wiki_id = $1 AND p.namespace = 'module' AND p.slug = $2 AND p.deleted_at IS NULL
           ORDER BY (COALESCE(p.locale, '') = w.default_locale) DESC, p.created_at
           LIMIT 1"#,
        wiki_id,
        module
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|r| (r.id, code_of(&r.body_md))))
}

/// What a module knows about the wiki: its name, language, today's date and
/// live statistics, counted at most once a minute.
async fn wiki_value(state: &AppState, ctx: &Ctx) -> Result<Value, AppError> {
    static STATS: LazyLock<Mutex<HashMap<Uuid, (Instant, Value)>>> =
        LazyLock::new(Default::default);
    let wiki_id = ctx.wiki.id;
    let kept = STATS.lock().ok().and_then(|m| {
        m.get(&wiki_id)
            .filter(|(at, _)| at.elapsed() < KEEP)
            .map(|(_, v)| v.clone())
    });
    let stats = match kept {
        Some(stats) => stats,
        None => {
            let row = sqlx::query!(
                r#"SELECT
                     (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main'
                       AND deleted_at IS NULL AND current_revision_id IS NOT NULL) AS "articles!",
                     (SELECT count(*) FROM pages WHERE wiki_id = $1 AND deleted_at IS NULL
                       AND current_revision_id IS NOT NULL) AS "pages!",
                     (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id
                       WHERE p.wiki_id = $1) AS "edits!",
                     (SELECT count(*) FROM media WHERE wiki_id = $1) AS "files!",
                     (SELECT count(DISTINCT r.author_id) FROM revisions r JOIN pages p ON p.id = r.page_id
                       WHERE p.wiki_id = $1) AS "editors!",
                     (SELECT count(DISTINCT r.author_id) FROM revisions r JOIN pages p ON p.id = r.page_id
                       WHERE p.wiki_id = $1 AND r.created_at > now() - interval '30 days') AS "active_editors!",
                     (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id
                       WHERE p.wiki_id = $1 AND r.created_at > now() - interval '30 days') AS "edits_month!""#,
                wiki_id
            )
            .fetch_one(&state.db)
            .await?;
            let stats = json!({
                "articles": row.articles,
                "pages": row.pages,
                "edits": row.edits,
                "files": row.files,
                "editors": row.editors,
                "activeEditors": row.active_editors,
                "editsMonth": row.edits_month,
            });
            if let Ok(mut m) = STATS.lock() {
                m.insert(wiki_id, (Instant::now(), stats.clone()));
            }
            stats
        }
    };
    Ok(json!({
        "name": ctx.wiki.name,
        "lang": ctx.content_locale,
        "today": chrono::Utc::now().format("%Y-%m-%d").to_string(),
        "stats": stats,
    }))
}

static RESULTS: LazyLock<Mutex<HashMap<String, (Instant, String)>>> =
    LazyLock::new(Default::default);

fn cached(key: &str) -> Option<String> {
    let m = RESULTS.lock().ok()?;
    m.get(key)
        .filter(|(at, _)| at.elapsed() < KEEP)
        .map(|(_, v)| v.clone())
}

fn remember(key: String, value: String) {
    if let Ok(mut m) = RESULTS.lock() {
        if m.len() >= RESULTS_MAX {
            m.retain(|_, (at, _)| at.elapsed() < KEEP);
            if m.len() >= RESULTS_MAX {
                m.clear();
            }
        }
        m.insert(key, (Instant::now(), value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_found_with_and_without_arguments() {
        let text = "Intro\n:::Module:Stats:summary\n\nMiddle\n:::module:stats:card\nstyle = big\nlabel = Pages\n:::\nEnd\n\
                    ```\n:::Module:Stats:summary\n```\n";
        let found = calls(text);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].module, "stats");
        assert_eq!(found[0].function, "summary");
        assert!(found[0].args.is_empty());
        assert_eq!((found[0].start, found[0].end), (1, 2));
        assert_eq!(found[1].function, "card");
        assert_eq!(
            found[1].args,
            vec![
                ("style".into(), "big".into()),
                ("label".into(), "Pages".into())
            ]
        );
        assert_eq!((found[1].start, found[1].end), (4, 8));
    }

    #[test]
    fn bad_names_are_not_calls() {
        assert_eq!(
            opener(":::Module:Stats:summary"),
            Some(("stats".into(), "summary".into()))
        );
        assert_eq!(opener(":::Module:../x:f"), None);
        assert_eq!(opener(":::Module:stats:f()"), None);
        assert_eq!(opener(":::details Module:stats:f"), None);
        assert_eq!(opener(":::Module:stats:"), None);
    }

    #[test]
    fn a_line_after_the_call_that_is_not_an_argument_stays_text() {
        let found =
            calls(":::Module:Stats:summary\nA sentence = not an argument, it has spaces.\n:::\n");
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].start, found[0].end), (0, 1));
    }

    #[test]
    fn the_code_is_the_first_js_block() {
        let body = "# Stats\n\nDocs.\n\n```text\nnot code\n```\n\n```js\nfunction summary() {\n  return 'hi';\n}\n```\n\n```js\nignored\n```\n";
        assert_eq!(
            code_of(body).unwrap(),
            "function summary() {\n  return 'hi';\n}"
        );
        assert_eq!(code_of("no code here"), None);
    }

    #[test]
    fn a_function_gets_its_arguments_the_wiki_and_the_page() {
        let code = "function card(args, wiki, page) {\n  return `**${fmt(wiki.stats.pages)}** ${args.label} on ${page.lang} (${plural(21, 'одна', 'две', 'много')})`;\n}";
        let out = run(
            code,
            "card",
            (
                json!({ "label": "pages" }),
                json!({ "stats": { "pages": 12345 } }),
                json!({ "lang": "ru" }),
            ),
        )
        .unwrap();
        assert_eq!(out, "**12\u{a0}345** pages on ru (одна)");
    }

    #[test]
    fn runaway_and_broken_code_is_stopped_with_a_reason() {
        let looped = run(
            "function f() { while (true) {} }",
            "f",
            (json!({}), json!({}), json!({})),
        );
        assert!(looped.is_err(), "{looped:?}");
        let deep = run(
            "function f() { return f(); }",
            "f",
            (json!({}), json!({}), json!({})),
        );
        assert!(deep.is_err(), "{deep:?}");
        let missing = run("var x = 1;", "f", (json!({}), json!({}), json!({})));
        assert_eq!(missing, Err("no function named f".into()));
        let syntax = run("function f( {", "f", (json!({}), json!({}), json!({})));
        assert!(syntax.is_err());
        let big = run(
            "function f() { return 'x'.repeat(70000); }",
            "f",
            (json!({}), json!({}), json!({})),
        );
        assert!(big.is_err(), "{big:?}");
    }

    #[test]
    fn there_is_no_way_out_of_the_sandbox() {
        for probe in [
            "typeof require",
            "typeof process",
            "typeof fetch",
            "typeof Deno",
            "typeof Bun",
        ] {
            let code = format!("function f() {{ return {probe}; }}");
            assert_eq!(
                run(&code, "f", (json!({}), json!({}), json!({}))),
                Ok("undefined".into()),
                "{probe}"
            );
        }
    }
}
