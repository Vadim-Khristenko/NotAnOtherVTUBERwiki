//! Diagrams in Markdown: ```mermaid and ```dot fences drawn by the worker.
//!
//! The Markdown pipeline leaves a fence as its source in a `<pre>`, so the
//! render cache never depends on the worker. After the cache, like emotes,
//! each fence whose drawings are ready becomes a figure with two images, one
//! per theme, and its source folded under them. A fence with no drawing yet
//! is queued and stays as text; a reader never waits on the worker.
//!
//! The runner is the only thing that talks to the worker, from the
//! background, with a timeout. Whatever the worker returns is rebuilt by
//! `naw_core::svg` before it is stored, and served with a CSP that forbids
//! scripts.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::resolve::Ctx;

/// Part of every diagram's hash. Bump it whenever the worker or the
/// sanitizer changes what a drawing looks like: the files are served as
/// immutable, so a browser keeps the old drawing under the old address.
const DRAWING_VERSION: &str = "1";
/// Retries for a worker that is down or failing, before a drawing gives up.
const MAX_ATTEMPTS: i32 = 5;
/// The worker gets this long per drawing; its own limit is shorter.
const WORKER_TIMEOUT: Duration = Duration::from_secs(40);
/// A claimed drawing older than this was lost with a restart.
const STALE_CLAIM_SECS: f64 = 180.0;
const THEMES: [&str; 2] = ["light", "dark"];

/// Fences the Markdown pipeline marks, with the language the worker knows.
const FENCES: &[(&str, &str)] = &[
    ("<pre class=\"mermaid\">", "mermaid"),
    ("<pre class=\"diagram diagram-dot\">", "dot"),
];

static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// One diagram fence in rendered HTML.
#[derive(Debug, PartialEq)]
struct Fence {
    /// Byte range of the whole `<pre>...</pre>`.
    start: usize,
    end: usize,
    lang: &'static str,
    source: String,
    hash: String,
}

fn hash(lang: &str, source: &str) -> String {
    let mut h = Sha256::new();
    h.update(DRAWING_VERSION.as_bytes());
    h.update(b"\n");
    h.update(lang.as_bytes());
    h.update(b"\n");
    h.update(source.as_bytes());
    hex::encode(h.finalize())
}

/// Every marked fence, in order.
fn fences(html: &str) -> Vec<Fence> {
    let mut found = Vec::new();
    for (open, lang) in FENCES {
        let mut from = 0;
        while let Some(at) = html[from..].find(open) {
            let start = from + at;
            let after_pre = start + open.len();
            let Some(code_open_end) = html[after_pre..]
                .strip_prefix("<code")
                .and_then(|rest| rest.find('>'))
                .map(|i| after_pre + "<code".len() + i + 1)
            else {
                from = after_pre;
                continue;
            };
            let Some(close) = html[code_open_end..].find("</code></pre>") else {
                from = after_pre;
                continue;
            };
            let inner = &html[code_open_end..code_open_end + close];
            let end = code_open_end + close + "</code></pre>".len();
            let source = unescape(inner);
            let source = source.trim_end_matches('\n').to_string();
            if !source.trim().is_empty() {
                found.push(Fence {
                    start,
                    end,
                    lang,
                    hash: hash(lang, &source),
                    source,
                });
            }
            from = end;
        }
    }
    found.sort_by_key(|f| f.start);
    found
}

/// Undoes the escaping the Markdown pipeline put on code text.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let Some(semi) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A drawing that is ready, for the figure.
struct Ready {
    width: i32,
    height: i32,
}

/// Turns fences with ready drawings into figures. With `queue`, fences never
/// seen before are queued for the worker; previews pass `false`, so unsaved
/// text cannot fill the queue.
pub(crate) async fn expand(
    state: &AppState,
    ctx: &Ctx,
    html: String,
    queue: bool,
) -> Result<String, AppError> {
    if !FENCES.iter().any(|(open, _)| html.contains(open)) {
        return Ok(html);
    }
    let found = fences(&html);
    if found.is_empty() {
        return Ok(html);
    }
    let hashes: Vec<String> = found
        .iter()
        .map(|f| f.hash.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let rows = sqlx::query!(
        "SELECT hash, theme, status, width, height FROM diagrams WHERE hash = ANY($1)",
        &hashes
    )
    .fetch_all(&state.db)
    .await?;
    let mut known: HashSet<String> = HashSet::new();
    let mut ready: HashMap<(String, String), Ready> = HashMap::new();
    for row in rows {
        known.insert(row.hash.clone());
        if row.status == "ready"
            && let (Some(width), Some(height)) = (row.width, row.height)
        {
            ready.insert((row.hash, row.theme), Ready { width, height });
        }
    }
    if queue && state.config.worker_url.is_some() {
        let new: Vec<&Fence> = found.iter().filter(|f| !known.contains(&f.hash)).collect();
        if !new.is_empty() {
            enqueue(&state.db, &new).await?;
        }
    }
    let alt = ctx.t("page.diagram_alt");
    let source_label = ctx.t("page.diagram_source");
    let mut out = String::with_capacity(html.len());
    let mut last = 0;
    for fence in &found {
        let light = ready.get(&(fence.hash.clone(), "light".to_string()));
        let dark = ready.get(&(fence.hash.clone(), "dark".to_string()));
        let (Some(light), Some(dark)) = (light, dark) else {
            continue;
        };
        out.push_str(&html[last..fence.start]);
        out.push_str("<figure class=\"diagram\">");
        // Each drawing links to its SVG, so without script a click still
        // opens it at full size; the skin's viewer takes the click over.
        // Only one theme shows, so both carry the same alt text.
        for (theme, drawing) in [("light", light), ("dark", dark)] {
            out.push_str(&format!(
                "<a class=\"diagram-open diagram-{theme}\" href=\"/media/diagrams/{hash}-{theme}.svg\"><img src=\"/media/diagrams/{hash}-{theme}.svg\" width=\"{}\" height=\"{}\" alt=\"{}\" loading=\"lazy\" decoding=\"async\"></a>",
                drawing.width,
                drawing.height,
                naw_core::html::escape(&alt),
                hash = fence.hash,
            ));
        }
        out.push_str("<details class=\"diagram-source\"><summary>");
        out.push_str(&naw_core::html::escape(&source_label));
        out.push_str("</summary>");
        out.push_str(&html[fence.start..fence.end]);
        out.push_str("</details></figure>");
        last = fence.end;
    }
    out.push_str(&html[last..]);
    Ok(out)
}

/// Queues the diagrams in a saved page's HTML, so they are drawn before the
/// first reader asks. Never fails a save.
pub(crate) async fn queue_in(state: &AppState, html: &str) {
    if state.config.worker_url.is_none() {
        return;
    }
    let found = fences(html);
    if found.is_empty() {
        return;
    }
    let refs: Vec<&Fence> = found.iter().collect();
    if let Err(err) = enqueue(&state.db, &refs).await {
        tracing::warn!(error = ?err, "could not queue diagrams");
    }
}

async fn enqueue(db: &sqlx::PgPool, fences: &[&Fence]) -> Result<(), AppError> {
    let mut hashes = Vec::new();
    let mut themes = Vec::new();
    let mut langs = Vec::new();
    let mut sources = Vec::new();
    let mut seen = HashSet::new();
    for fence in fences {
        if !seen.insert(fence.hash.as_str()) {
            continue;
        }
        for theme in THEMES {
            hashes.push(fence.hash.clone());
            themes.push(theme.to_string());
            langs.push(fence.lang.to_string());
            sources.push(fence.source.clone());
        }
    }
    let inserted = sqlx::query!(
        "INSERT INTO diagrams (hash, theme, lang, source)
         SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[])
         ON CONFLICT DO NOTHING",
        &hashes,
        &themes,
        &langs,
        &sources
    )
    .execute(db)
    .await?
    .rows_affected();
    if inserted > 0 {
        WAKE.notify_one();
    }
    Ok(())
}

/// Starts the runner when a worker is configured.
pub fn start(state: AppState) {
    let Some(url) = state.config.worker_url.clone() else {
        tracing::info!("no worker configured; diagrams stay as their source");
        return;
    };
    tokio::spawn(run(state, url));
}

enum Outcome {
    Drawn(naw_core::svg::Clean),
    /// The source cannot be drawn; retrying will not help.
    Refused(String),
    /// The worker broke on it; try again later, a limited number of times.
    Retry(String),
    /// The worker is not there; wait for it without spending an attempt.
    Away(String),
}

async fn run(state: AppState, url: String) {
    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(WORKER_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::error!(error = %err, "diagram runner could not build its client");
            return;
        }
    };
    let endpoint = format!("{}/render/diagram", url.trim_end_matches('/'));
    let mut backoff = Duration::from_secs(5);
    loop {
        match claim(&state.db).await {
            Ok(Some(job)) => {
                let outcome = draw(&client, &endpoint, &job).await;
                let retrying = matches!(outcome, Outcome::Retry(_) | Outcome::Away(_));
                if let Err(err) = settle(&state.db, &job, outcome).await {
                    tracing::warn!(error = ?err, hash = %job.hash, "could not store a diagram");
                }
                if retrying {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                } else {
                    backoff = Duration::from_secs(5);
                }
            }
            Ok(None) => {
                let _ = tokio::time::timeout(Duration::from_secs(30), WAKE.notified()).await;
            }
            Err(err) => {
                tracing::warn!(error = ?err, "diagram queue unavailable");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
}

struct Job {
    hash: String,
    theme: String,
    lang: String,
    source: String,
    attempts: i32,
}

/// Takes the oldest pending drawing, after putting back claims a restart lost.
async fn claim(db: &sqlx::PgPool) -> Result<Option<Job>, AppError> {
    sqlx::query!(
        "UPDATE diagrams SET status = 'pending'
         WHERE status = 'rendering' AND claimed_at < now() - make_interval(secs => $1)",
        STALE_CLAIM_SECS
    )
    .execute(db)
    .await?;
    let row = sqlx::query!(
        // A CTE runs once; a subquery joined into the UPDATE can run per row
        // and claim the whole queue in one statement.
        r#"WITH next AS MATERIALIZED (
               SELECT hash, theme FROM diagrams WHERE status = 'pending'
               ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED)
           UPDATE diagrams d SET status = 'rendering', claimed_at = now(), attempts = d.attempts + 1
           FROM next
           WHERE d.hash = next.hash AND d.theme = next.theme
           RETURNING d.hash, d.theme, d.lang, d.source, d.attempts"#
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Job {
        hash: r.hash,
        theme: r.theme,
        lang: r.lang,
        source: r.source,
        attempts: r.attempts,
    }))
}

async fn draw(client: &reqwest::Client, endpoint: &str, job: &Job) -> Outcome {
    let response = client
        .post(endpoint)
        .json(&serde_json::json!({
            "lang": job.lang,
            "theme": job.theme,
            "source": job.source,
        }))
        .send()
        .await;
    let response = match response {
        Ok(r) => r,
        Err(err) if err.is_connect() => {
            return Outcome::Away(format!("worker unreachable: {err}"));
        }
        Err(err) => return Outcome::Retry(format!("worker failed: {err}")),
    };
    let status = response.status();
    let body: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(err) => return Outcome::Retry(format!("worker answered {status} without JSON: {err}")),
    };
    let message = || {
        body.get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("no reason given")
            .chars()
            .take(500)
            .collect::<String>()
    };
    if status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST {
        return Outcome::Refused(message());
    }
    if !status.is_success() {
        return Outcome::Retry(format!("worker answered {status}: {}", message()));
    }
    let Some(svg) = body.get("svg").and_then(|s| s.as_str()) else {
        return Outcome::Retry("worker answered without a drawing".into());
    };
    match naw_core::svg::sanitize(svg) {
        Ok(clean) => Outcome::Drawn(clean),
        Err(err) => Outcome::Refused(format!("the drawing was refused: {err}")),
    }
}

async fn settle(db: &sqlx::PgPool, job: &Job, outcome: Outcome) -> Result<(), AppError> {
    match outcome {
        Outcome::Drawn(clean) => {
            sqlx::query!(
                "UPDATE diagrams SET status = 'ready', svg = $3, width = $4, height = $5,
                     error = NULL, rendered_at = now()
                 WHERE hash = $1 AND theme = $2",
                job.hash,
                job.theme,
                clean.svg,
                clean.width as i32,
                clean.height as i32
            )
            .execute(db)
            .await?;
        }
        Outcome::Refused(error) => fail(db, job, &error).await?,
        Outcome::Retry(error) if job.attempts >= MAX_ATTEMPTS => fail(db, job, &error).await?,
        Outcome::Retry(error) => {
            tracing::warn!(hash = %job.hash, theme = %job.theme, %error, "diagram will be retried");
            sqlx::query!(
                "UPDATE diagrams SET status = 'pending', error = $3 WHERE hash = $1 AND theme = $2",
                job.hash,
                job.theme,
                error
            )
            .execute(db)
            .await?;
        }
        Outcome::Away(error) => {
            tracing::warn!(%error, "diagram worker is away");
            sqlx::query!(
                "UPDATE diagrams SET status = 'pending', error = $3, attempts = attempts - 1
                 WHERE hash = $1 AND theme = $2",
                job.hash,
                job.theme,
                error
            )
            .execute(db)
            .await?;
        }
    }
    Ok(())
}

async fn fail(db: &sqlx::PgPool, job: &Job, error: &str) -> Result<(), AppError> {
    tracing::info!(hash = %job.hash, theme = %job.theme, %error, "diagram cannot be drawn");
    sqlx::query!(
        "UPDATE diagrams SET status = 'failed', error = $3 WHERE hash = $1 AND theme = $2",
        job.hash,
        job.theme,
        error
    )
    .execute(db)
    .await?;
    Ok(())
}

/// `GET /media/diagrams/{hash}-{theme}.svg`. Content addressed, so it is
/// cached for good; the CSP forbids scripts even when opened on its own.
pub async fn serve(
    State(state): State<AppState>,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some((hash, theme)) = file
        .strip_suffix(".svg")
        .and_then(|stem| stem.rsplit_once('-'))
    else {
        return Ok(crate::errors::not_found());
    };
    let hex = hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex || !THEMES.contains(&theme) {
        return Ok(crate::errors::not_found());
    }
    let etag = format!("\"{hash}-{theme}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let Some(svg) = sqlx::query_scalar!(
        "SELECT svg FROM diagrams WHERE hash = $1 AND theme = $2 AND status = 'ready'",
        hash,
        theme
    )
    .fetch_optional(&state.db)
    .await?
    .flatten() else {
        return Ok(crate::errors::not_found());
    };
    let mut response = svg.into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/svg+xml; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, value);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_fences_the_markdown_pipeline_marks() {
        let html = naw_markdown::render_html(
            "Before\n\n```mermaid\ngraph TD\n  A[\"Filian & co\"] --> B<br>\n```\n\n```dot\ndigraph { a -> b }\n```\n\n```rust\nfn main() {}\n```\n",
        );
        let found = fences(&html);
        assert_eq!(found.len(), 2, "{html}");
        assert_eq!(found[0].lang, "mermaid");
        assert_eq!(found[0].source, "graph TD\n  A[\"Filian & co\"] --> B<br>");
        assert_eq!(found[1].lang, "dot");
        assert_eq!(found[1].source, "digraph { a -> b }");
        assert_eq!(&html[found[0].start..found[0].start + 5], "<pre ");
        assert!(html[..found[0].end].ends_with("</code></pre>"));
        assert_ne!(found[0].hash, found[1].hash);
    }

    #[test]
    fn graphviz_fences_share_the_dot_language() {
        let a = fences(&naw_markdown::render_html(
            "```graphviz\ndigraph { a }\n```\n",
        ));
        let b = fences(&naw_markdown::render_html("```dot\ndigraph { a }\n```\n"));
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].hash, b[0].hash);
    }

    #[test]
    fn an_empty_fence_is_left_alone() {
        assert!(fences(&naw_markdown::render_html("```mermaid\n\n```\n")).is_empty());
    }

    async fn queue(db: &sqlx::PgPool, markdown: &str) {
        let html = naw_markdown::render_html(markdown);
        let found = fences(&html);
        let refs: Vec<&Fence> = found.iter().collect();
        enqueue(db, &refs).await.expect("enqueue");
    }

    async fn statuses(db: &sqlx::PgPool) -> Vec<String> {
        sqlx::query_scalar!("SELECT status FROM diagrams ORDER BY status")
            .fetch_all(db)
            .await
            .expect("q")
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_claim_takes_one_drawing_at_a_time(db: sqlx::PgPool) {
        queue(
            &db,
            "```mermaid\ngraph TD; a-->b\n```\n\n```dot\ndigraph { a }\n```\n",
        )
        .await;
        // queuing the same text again adds nothing
        queue(&db, "```dot\ndigraph { a }\n```\n").await;
        assert_eq!(statuses(&db).await.len(), 4);
        let first = claim(&db).await.expect("claim").expect("a job");
        assert_eq!(first.attempts, 1);
        let mut s = statuses(&db).await;
        s.dedup();
        assert_eq!(s, ["pending", "rendering"]);
        assert_eq!(
            statuses(&db)
                .await
                .iter()
                .filter(|s| *s == "rendering")
                .count(),
            1
        );
        let second = claim(&db).await.expect("claim").expect("a job");
        assert_ne!((&first.hash, &first.theme), (&second.hash, &second.theme));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn outcomes_settle_a_drawing(db: sqlx::PgPool) {
        queue(&db, "```dot\ndigraph { a }\n```\n").await;
        let job = claim(&db).await.expect("claim").expect("a job");
        let clean = naw_core::svg::sanitize(r#"<svg viewBox="0 0 10 20"><g/></svg>"#).unwrap();
        settle(&db, &job, Outcome::Drawn(clean))
            .await
            .expect("settle");
        let job = claim(&db).await.expect("claim").expect("the other theme");
        settle(&db, &job, Outcome::Away("down".into()))
            .await
            .expect("settle");
        // an absent worker costs no attempt
        let job = claim(&db).await.expect("claim").expect("again");
        assert_eq!(job.attempts, 1);
        settle(&db, &job, Outcome::Refused("bad".into()))
            .await
            .expect("settle");
        assert_eq!(statuses(&db).await, ["failed", "ready"]);
        assert!(claim(&db).await.expect("claim").is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_lost_claim_goes_back_to_the_queue(db: sqlx::PgPool) {
        queue(&db, "```dot\ndigraph { a }\n```\n").await;
        claim(&db).await.expect("claim").expect("a job");
        sqlx::query!(
            "UPDATE diagrams SET claimed_at = now() - interval '1 hour' WHERE status = 'rendering'"
        )
        .execute(&db)
        .await
        .expect("age");
        // the stale claim is put back and taken again, plus the other theme
        assert!(claim(&db).await.expect("claim").is_some());
        assert!(claim(&db).await.expect("claim").is_some());
        assert!(claim(&db).await.expect("claim").is_none());
    }

    #[test]
    fn unescape_handles_named_and_numeric_references() {
        assert_eq!(
            unescape("a &lt;b&gt; &amp; &#39;c&#x27; &quot;"),
            "a <b> & 'c' \""
        );
        assert_eq!(unescape("& alone &bogus;"), "& alone &bogus;");
    }
}
