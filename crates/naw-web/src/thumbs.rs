//! Smaller WebP copies of uploaded pictures, for phones and narrow columns.
//!
//! After the cache, like emotes and diagrams, each uploaded JPEG, PNG or
//! WebP picture wider than the smallest size gains a `srcset` of WebP copies
//! at 480, 960 and 1600 pixels, plus the original. A copy that does not exist
//! yet is queued on its first request, and that request is redirected to the
//! original, so a reader never waits on the worker. Without a worker nothing
//! changes: no `srcset`, no queue.
//!
//! The worker draws the copies; the engine checks each answer is a WebP of
//! the right width before it stores it under `thumbs/`, and serves it with
//! the same locked-down headers as any upload. A hidden file has no copies.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::Notify;

use naw_core::error::AppError;
use naw_core::state::AppState;

/// The widths a copy may have.
const WIDTHS: [u32; 3] = [480, 960, 1600];
/// Retries for a worker that is failing, before a copy gives up.
const MAX_ATTEMPTS: i32 = 4;
const WORKER_TIMEOUT: Duration = Duration::from_secs(60);
const STALE_CLAIM_SECS: f64 = 300.0;
/// Largest original the worker is asked to shrink.
const SOURCE_MAX: usize = 25 * 1024 * 1024;
/// How wide an article picture shows: the whole screen on a phone, the
/// text column (44rem) beside it.
const SIZES: &str = "(max-width: 48rem) 100vw, 44rem";

static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// `/media/ab/<64 hex>.<jpg|jpeg|png|webp>` split into its hex stem, or
/// `None` for any other address.
fn source_stem(src: &str) -> Option<&str> {
    let rest = src.strip_prefix("/media/")?;
    let (prefix, file) = rest.split_once('/')?;
    let (stem, ext) = file.rsplit_once('.')?;
    let hex = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let ok = prefix.len() == 2
        && hex(prefix)
        && stem.len() == 64
        && hex(stem)
        && stem.starts_with(prefix)
        && matches!(ext, "jpg" | "jpeg" | "png" | "webp");
    ok.then_some(stem)
}

fn thumb_url(stem: &str, width: u32) -> String {
    format!("/media/thumbs/{stem}-{width}.webp")
}

fn thumb_key(stem: &str, width: u32) -> String {
    format!("thumbs/{stem}-{width}.webp")
}

/// The `srcset` for a picture this wide, or `None` when it is already small.
fn srcset(src: &str, stem: &str, width: u32) -> Option<String> {
    let mut parts: Vec<String> = WIDTHS
        .iter()
        .filter(|w| **w < width)
        .map(|w| format!("{} {w}w", thumb_url(stem, *w)))
        .collect();
    if parts.is_empty() {
        return None;
    }
    parts.push(format!("{src} {width}w"));
    Some(parts.join(", "))
}

/// Adds a `srcset` to every uploaded picture that is wider than the
/// smallest copy. Cheap when there is nothing to do.
pub(crate) async fn expand(state: &AppState, html: String) -> Result<String, AppError> {
    if state.config.worker_url.is_none() || !html.contains("src=\"/media/") {
        return Ok(html);
    }
    let mut srcs: Vec<String> = Vec::new();
    let mut rest = html.as_str();
    while let Some(at) = rest.find("<img ") {
        let tag_end = rest[at..].find('>').map_or(rest.len(), |e| at + e);
        let tag = &rest[at..tag_end];
        if let Some(src) = attr(tag, "src")
            && source_stem(src).is_some()
            && !tag.contains(" srcset=")
        {
            srcs.push(format!("media/{}", &src["/media/".len()..]));
        }
        rest = &rest[tag_end..];
    }
    if srcs.is_empty() {
        return Ok(html);
    }
    srcs.sort();
    srcs.dedup();
    let widths: HashMap<String, i32> = sqlx::query!(
        "SELECT storage_key, width FROM media_versions WHERE storage_key = ANY($1) AND width IS NOT NULL",
        &srcs
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .filter_map(|r| Some((r.storage_key, r.width?)))
    .collect();
    let mut out = String::with_capacity(html.len() + widths.len() * 200);
    let mut rest = html.as_str();
    while let Some(at) = rest.find("<img ") {
        out.push_str(&rest[..at]);
        let tag_end = rest[at..].find('>').map_or(rest.len(), |e| at + e);
        let tag = &rest[at..tag_end];
        let set = attr(tag, "src").and_then(|src| {
            let stem = source_stem(src)?;
            let width = *widths.get(&format!("media/{}", &src["/media/".len()..]))?;
            if tag.contains(" srcset=") {
                return None;
            }
            srcset(src, stem, u32::try_from(width).ok()?)
        });
        match set {
            Some(set) => {
                out.push_str("<img srcset=\"");
                out.push_str(&set);
                out.push_str("\" sizes=\"");
                out.push_str(SIZES);
                out.push_str("\" ");
                out.push_str(&tag["<img ".len()..]);
            }
            None => out.push_str(tag),
        }
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `name="value"` from a tag; the first match wins.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')?;
    Some(&tag[start..start + end])
}

/// `GET /media/thumbs/<stem>-<width>.webp`: the copy when it is ready,
/// else a redirect to the original while the copy is queued.
pub async fn serve(
    State(state): State<AppState>,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some((stem, width)) = file
        .strip_suffix(".webp")
        .and_then(|s| s.rsplit_once('-'))
        .and_then(|(stem, w)| Some((stem, w.parse::<u32>().ok()?)))
    else {
        return Ok(crate::errors::not_found());
    };
    let hex = stem.len() == 64
        && stem
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex || !WIDTHS.contains(&width) {
        return Ok(crate::errors::not_found());
    }
    // The original: any stored version with this hash.
    let pattern = format!("media/{}/{stem}.%", &stem[..2]);
    let Some(source) = sqlx::query_scalar!(
        "SELECT storage_key FROM media_versions WHERE storage_key LIKE $1 LIMIT 1",
        pattern
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    if source_stem(&crate::media::url_for_key(&source)).is_none()
        || !crate::file_actions::servable(&state.db, &source).await?
    {
        return Ok(crate::errors::not_found());
    }
    let etag = format!("\"{stem}-{width}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let ready = sqlx::query_scalar!(
        "SELECT status FROM image_variants WHERE source_key = $1 AND width = $2",
        source,
        width as i32
    )
    .fetch_optional(&state.db)
    .await?;
    if ready.as_deref() == Some("ready")
        && let Some(bytes) = state.storage.get(&thumb_key(stem, width)).await?
    {
        let mut response = Response::new(Body::from(bytes));
        let h = response.headers_mut();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; sandbox"),
        );
        h.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        if let Ok(value) = HeaderValue::from_str(&etag) {
            h.insert(header::ETAG, value);
        }
        return Ok(response);
    }
    if ready.is_none() && state.config.worker_url.is_some() {
        let inserted = sqlx::query!(
            "INSERT INTO image_variants (source_key, width) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            source,
            width as i32
        )
        .execute(&state.db)
        .await?
        .rows_affected();
        if inserted > 0 {
            WAKE.notify_one();
        }
    }
    let mut response = StatusCode::FOUND.into_response();
    let h = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&crate::media::url_for_key(&source)) {
        h.insert(header::LOCATION, value);
    }
    // The copy may be ready on the next visit.
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Starts the runner when a worker is configured.
pub fn start(state: AppState) {
    let Some(url) = state.config.worker_url.clone() else {
        return;
    };
    tokio::spawn(run(state, url));
}

enum Outcome {
    Drawn(Vec<u8>),
    /// The picture cannot be shrunk; retrying will not help.
    Refused(String),
    /// The worker broke on it; try again later, a limited number of times.
    Retry(String),
    /// The worker is not there; wait for it without spending an attempt.
    Away(String),
}

struct Job {
    source: String,
    width: i32,
    attempts: i32,
}

async fn run(state: AppState, url: String) {
    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(WORKER_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::error!(error = %err, "thumbnail runner could not build its client");
            return;
        }
    };
    let endpoint = format!("{}/render/image", url.trim_end_matches('/'));
    let mut backoff = Duration::from_secs(5);
    loop {
        match claim(&state.db).await {
            Ok(Some(job)) => {
                let outcome = draw(&state, &client, &endpoint, &job).await;
                let waiting = matches!(outcome, Outcome::Retry(_) | Outcome::Away(_));
                if let Err(err) = settle(&state, &job, outcome).await {
                    tracing::warn!(error = ?err, source = %job.source, "could not store a thumbnail");
                }
                if waiting {
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
                tracing::warn!(error = ?err, "thumbnail queue unavailable");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
}

async fn claim(db: &sqlx::PgPool) -> Result<Option<Job>, AppError> {
    sqlx::query!(
        "UPDATE image_variants SET status = 'pending'
         WHERE status = 'rendering' AND claimed_at < now() - make_interval(secs => $1)",
        STALE_CLAIM_SECS
    )
    .execute(db)
    .await?;
    let row = sqlx::query!(
        // A CTE runs once, so one statement claims one copy.
        r#"WITH next AS MATERIALIZED (
               SELECT source_key, width FROM image_variants WHERE status = 'pending'
               ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED)
           UPDATE image_variants v SET status = 'rendering', claimed_at = now(), attempts = v.attempts + 1
           FROM next
           WHERE v.source_key = next.source_key AND v.width = next.width
           RETURNING v.source_key, v.width, v.attempts"#
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Job {
        source: r.source_key,
        width: r.width,
        attempts: r.attempts,
    }))
}

async fn draw(state: &AppState, client: &reqwest::Client, endpoint: &str, job: &Job) -> Outcome {
    let original = match state.storage.get(&job.source).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Outcome::Refused("the original is gone".into()),
        Err(err) => return Outcome::Retry(format!("storage: {err:?}")),
    };
    if original.len() > SOURCE_MAX {
        return Outcome::Refused("the original is too large to shrink".into());
    }
    let response = client
        .post(format!("{endpoint}?width={}", job.width))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(original)
        .send()
        .await;
    let response = match response {
        Ok(r) => r,
        Err(err) if err.is_connect() => return Outcome::Away(format!("worker unreachable: {err}")),
        Err(err) => return Outcome::Retry(format!("worker failed: {err}")),
    };
    let status = response.status();
    let bytes = match response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(err) => {
            return Outcome::Retry(format!("worker answered {status} and broke off: {err}"));
        }
    };
    if status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST {
        return Outcome::Refused(String::from_utf8_lossy(&bytes).chars().take(300).collect());
    }
    if !status.is_success() {
        return Outcome::Retry(format!("worker answered {status}"));
    }
    match webp_width(&bytes) {
        Some(w) if w > 0 && w <= job.width as u32 => Outcome::Drawn(bytes),
        Some(w) => Outcome::Refused(format!("the copy is {w} wide, not at most {}", job.width)),
        None => Outcome::Refused("the worker's answer is not a WebP picture".into()),
    }
}

/// The width of a WebP image from its header, or `None` when the bytes are
/// not WebP.
fn webp_width(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 30 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    match &bytes[12..16] {
        b"VP8X" => Some(1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0])),
        b"VP8 " => Some(u32::from(
            u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3fff,
        )),
        b"VP8L" => {
            let b = &bytes[21..25];
            Some(1 + (u32::from(b[0]) | (u32::from(b[1] & 0x3f) << 8)))
        }
        _ => None,
    }
}

async fn settle(state: &AppState, job: &Job, outcome: Outcome) -> Result<(), AppError> {
    let db = &state.db;
    match outcome {
        Outcome::Drawn(bytes) => {
            let size = bytes.len() as i32;
            let stem = job
                .source
                .rsplit_once('/')
                .and_then(|(_, f)| f.rsplit_once('.'))
                .map(|(s, _)| s.to_string())
                .unwrap_or_default();
            state
                .storage
                .put(&thumb_key(&stem, job.width as u32), bytes)
                .await?;
            sqlx::query!(
                "UPDATE image_variants SET status = 'ready', bytes = $3, error = NULL, rendered_at = now()
                 WHERE source_key = $1 AND width = $2",
                job.source,
                job.width,
                size
            )
            .execute(db)
            .await?;
        }
        Outcome::Refused(error) => fail(db, job, &error).await?,
        Outcome::Retry(error) if job.attempts >= MAX_ATTEMPTS => fail(db, job, &error).await?,
        Outcome::Retry(error) => {
            tracing::warn!(source = %job.source, width = job.width, %error, "thumbnail will be retried");
            sqlx::query!(
                "UPDATE image_variants SET status = 'pending', error = $3 WHERE source_key = $1 AND width = $2",
                job.source,
                job.width,
                error
            )
            .execute(db)
            .await?;
        }
        Outcome::Away(error) => {
            tracing::warn!(%error, "thumbnail worker is away");
            sqlx::query!(
                "UPDATE image_variants SET status = 'pending', error = $3, attempts = attempts - 1
                 WHERE source_key = $1 AND width = $2",
                job.source,
                job.width,
                error
            )
            .execute(db)
            .await?;
        }
    }
    Ok(())
}

async fn fail(db: &sqlx::PgPool, job: &Job, error: &str) -> Result<(), AppError> {
    tracing::info!(source = %job.source, width = job.width, %error, "thumbnail cannot be made");
    sqlx::query!(
        "UPDATE image_variants SET status = 'failed', error = $3 WHERE source_key = $1 AND width = $2",
        job.source,
        job.width,
        error
    )
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEM: &str = "ab00000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn only_uploaded_rasters_have_a_stem() {
        assert_eq!(source_stem(&format!("/media/ab/{STEM}.png")), Some(STEM));
        assert_eq!(source_stem(&format!("/media/ab/{STEM}.gif")), None);
        assert_eq!(source_stem(&format!("/media/cd/{STEM}.png")), None);
        assert_eq!(source_stem(&format!("/media/emotes/{STEM}.webp")), None);
        assert_eq!(source_stem("/media/ab/short.png"), None);
    }

    #[test]
    fn srcset_lists_the_smaller_copies_and_the_original() {
        let src = format!("/media/ab/{STEM}.jpg");
        assert_eq!(srcset(&src, STEM, 400), None);
        let set = srcset(&src, STEM, 1200).unwrap();
        assert_eq!(
            set,
            format!(
                "/media/thumbs/{STEM}-480.webp 480w, /media/thumbs/{STEM}-960.webp 960w, {src} 1200w"
            )
        );
    }

    #[test]
    fn webp_widths_are_read_from_each_header_kind() {
        // VP8X: width-1 in three bytes at 24.
        let mut x = vec![0u8; 30];
        x[0..4].copy_from_slice(b"RIFF");
        x[8..12].copy_from_slice(b"WEBP");
        x[12..16].copy_from_slice(b"VP8X");
        x[24..27].copy_from_slice(&[0xdf, 0x01, 0x00]); // 479 + 1
        assert_eq!(webp_width(&x), Some(480));
        let mut l = x.clone();
        l[12..16].copy_from_slice(b"VP8L");
        l[21] = 0xdf;
        l[22] = 0x01; // 0x1df + 1 = 480
        assert_eq!(webp_width(&l), Some(480));
        let mut v = x.clone();
        v[12..16].copy_from_slice(b"VP8 ");
        v[26..28].copy_from_slice(&480u16.to_le_bytes());
        assert_eq!(webp_width(&v), Some(480));
        assert_eq!(webp_width(b"\x89PNG not webp at all, really long"), None);
    }
}
