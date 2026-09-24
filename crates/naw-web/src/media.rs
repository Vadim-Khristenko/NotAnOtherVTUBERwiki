//! Uploaded files: storing, serving, and the upload page.
//!
//! The type comes from the first bytes only: images (PNG, JPEG, GIF, WebP,
//! AVIF), audio (MP3, Ogg, Opus, FLAC, WAV, M4A), video (MP4, WebM) and PDF,
//! never SVG or HTML, which can carry script. Avatars and emotes take images
//! only. Files are keyed by their SHA-256, so a URL never changes meaning and
//! is cached for a year. Served with `nosniff`, a CSP that allows nothing, and
//! PDFs as downloads.

use axum::body::Body;
use axum::extract::{Extension, Multipart, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, ENGINE_VERSION, template_error};
use crate::perm::Capability;

/// A file type this engine accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kind {
    pub mime: &'static str,
    pub ext: &'static str,
    /// `image`, `audio`, `video` or `document`: the page prefix and the player.
    pub class: &'static str,
}

/// Every type, by extension. `sniff` hands out only these, and serving
/// answers only for these.
const KINDS: &[Kind] = &[
    Kind {
        mime: "image/png",
        ext: "png",
        class: "image",
    },
    Kind {
        mime: "image/jpeg",
        ext: "jpg",
        class: "image",
    },
    Kind {
        mime: "image/gif",
        ext: "gif",
        class: "image",
    },
    Kind {
        mime: "image/webp",
        ext: "webp",
        class: "image",
    },
    Kind {
        mime: "image/avif",
        ext: "avif",
        class: "image",
    },
    Kind {
        mime: "audio/mpeg",
        ext: "mp3",
        class: "audio",
    },
    Kind {
        mime: "audio/ogg",
        ext: "ogg",
        class: "audio",
    },
    Kind {
        mime: "audio/opus",
        ext: "opus",
        class: "audio",
    },
    Kind {
        mime: "audio/flac",
        ext: "flac",
        class: "audio",
    },
    Kind {
        mime: "audio/wav",
        ext: "wav",
        class: "audio",
    },
    Kind {
        mime: "audio/mp4",
        ext: "m4a",
        class: "audio",
    },
    Kind {
        mime: "video/mp4",
        ext: "mp4",
        class: "video",
    },
    Kind {
        mime: "video/webm",
        ext: "webm",
        class: "video",
    },
    Kind {
        mime: "application/pdf",
        ext: "pdf",
        class: "document",
    },
];

fn kind_of(ext: &str) -> Option<Kind> {
    KINDS.iter().copied().find(|k| k.ext == ext)
}

/// The type from the file's first bytes, never from its name. Never SVG or
/// HTML, which can carry script.
pub fn sniff(data: &[u8]) -> Option<Kind> {
    let at = |range: std::ops::Range<usize>| data.get(range).unwrap_or_default();
    let ext = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        "gif"
    } else if at(0..4) == b"RIFF" && at(8..12) == b"WEBP" {
        "webp"
    } else if at(0..4) == b"RIFF" && at(8..12) == b"WAVE" {
        "wav"
    } else if at(4..8) == b"ftyp" {
        match at(8..12) {
            b"avif" | b"avis" => "avif",
            b"M4A " => "m4a",
            b"isom" | b"iso2" | b"iso4" | b"iso5" | b"iso6" | b"mp41" | b"mp42" | b"avc1"
            | b"dash" | b"MSNV" => "mp4",
            _ => return None,
        }
    } else if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        "webm"
    } else if data.starts_with(b"OggS") {
        // An Ogg page names its codec in the first packet.
        if data.windows(8).take(80).any(|w| w == b"OpusHead") {
            "opus"
        } else {
            "ogg"
        }
    } else if data.starts_with(b"fLaC") {
        "flac"
    } else if data.starts_with(b"ID3")
        || (data.len() > 1 && data[0] == 0xFF && matches!(data[1], 0xFB | 0xF3 | 0xF2))
    {
        "mp3"
    } else if data.starts_with(b"%PDF-") {
        "pdf"
    } else {
        return None;
    };
    kind_of(ext)
}

/// The served type, from an extension `sniff` handed out.
fn mime_for(file: &str) -> Option<Kind> {
    kind_of(file.rsplit_once('.')?.1)
}

/// A stored image, ready to be referenced.
pub struct Stored {
    pub key: String,
    pub url: String,
    pub kind: Kind,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Empty,
    TooLarge,
    NotAnImage,
    /// An image header with absurd dimensions.
    BadDimensions,
    /// The uploader's allowance is used up for now.
    Quota,
    /// An import URL that is not a public http(s) address.
    Address,
    /// An import that could not be downloaded.
    Unreachable,
}

impl Refusal {
    /// Suffix shared by `media.refused_*` and `settings.avatar_*`.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooLarge => "too_large",
            Self::NotAnImage => "type",
            Self::BadDimensions => "dimensions",
            Self::Quota => "quota",
            Self::Address => "address",
            Self::Unreachable => "unreachable",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Quota => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

/// Daily allowance per account and wiki, by files and bytes; wiki admins
/// are exempt.
const DAILY_UPLOADS: i64 = 300;
const DAILY_UPLOAD_BYTES: i64 = 2 * 1024 * 1024 * 1024;

async fn within_daily_quota(state: &AppState, ctx: &crate::resolve::Ctx) -> Result<bool, AppError> {
    if ctx.actor.can(Capability::WikiSettings) {
        return Ok(true);
    }
    let Some(me) = ctx.actor.user_id else {
        return Ok(false);
    };
    let used = sqlx::query!(
        r#"SELECT count(*) AS "files!", COALESCE(sum(size_bytes), 0)::bigint AS "bytes!"
           FROM media WHERE wiki_id = $1 AND uploader_id = $2 AND created_at > now() - interval '1 day'"#,
        ctx.wiki.id,
        me
    )
    .fetch_one(&state.db)
    .await?;
    Ok(used.files < DAILY_UPLOADS && used.bytes < DAILY_UPLOAD_BYTES)
}

/// The URL of a storage key; every key is served under `/media/`.
pub fn url_for_key(key: &str) -> String {
    match key.strip_prefix("media/") {
        Some(rest) => format!("/media/{rest}"),
        None => format!("/media/{key}"),
    }
}

/// Checks and stores an image under `prefix` (`media`, `avatars`).
pub async fn store(
    state: &AppState,
    prefix: &str,
    data: Vec<u8>,
    max: usize,
) -> Result<Result<Stored, Refusal>, AppError> {
    if data.is_empty() {
        return Ok(Err(Refusal::Empty));
    }
    if data.len() > max {
        return Ok(Err(Refusal::TooLarge));
    }
    let Some(kind) = sniff(&data).filter(|k| prefix == "media" || k.class == "image") else {
        return Ok(Err(Refusal::NotAnImage));
    };
    let size = (kind.class == "image")
        .then(|| imagesize::blob_size(&data).ok())
        .flatten();
    // A huge canvas makes every reader's browser allocate gigabytes.
    if let Some(s) = size
        && (s.width == 0 || s.height == 0 || s.width > 16384 || s.height > 16384)
    {
        return Ok(Err(Refusal::BadDimensions));
    }
    let hash = hex::encode(Sha256::digest(&data));
    let key = if prefix == "media" {
        format!("media/{}/{hash}.{}", &hash[..2], kind.ext)
    } else {
        format!("{prefix}/{hash}.{}", kind.ext)
    };
    let url = url_for_key(&key);
    let len = data.len();
    if !state.storage.exists(&key).await? {
        state.storage.put(&key, data).await?;
    }
    Ok(Ok(Stored {
        key,
        url,
        kind,
        width: size.map(|s| s.width as u32),
        height: size.map(|s| s.height as u32),
        size: len,
    }))
}

/// Reads the one file field of a multipart upload, stopping past `max`
/// instead of buffering the whole upload.
pub async fn read_file_field(
    multipart: &mut Multipart,
    field_name: &str,
    max: usize,
) -> Result<Option<(String, Vec<u8>)>, Refusal> {
    while let Ok(Some(mut field)) = multipart.next_field().await {
        if field.name() != Some(field_name) {
            continue;
        }
        let filename = field.file_name().unwrap_or("image").to_string();
        let mut data = Vec::new();
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    if data.len() + chunk.len() > max {
                        return Err(Refusal::TooLarge);
                    }
                    data.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => return Err(Refusal::TooLarge),
            }
        }
        return Ok(Some((filename, data)));
    }
    Ok(None)
}

/// The base name without control characters, at most 120 characters.
fn clean_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(120).collect();
    if cleaned.trim().is_empty() {
        "image".to_string()
    } else {
        cleaned.trim().to_string()
    }
}

/// The Markdown to paste for a file: `![Alt](image:name.png)`, by its page name,
/// with the alt text from the name it was uploaded under.
fn markdown_for(class: &str, name: &str, uploaded_as: &str) -> String {
    let alt: String = uploaded_as
        .rsplit_once('.')
        .map_or(uploaded_as, |(stem, _)| stem)
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '(' | ')'))
        .collect();
    let prefix = crate::files::prefix_of(class);
    if class == "document" {
        format!("[{}]({prefix}:{name})", alt.trim())
    } else {
        format!("![{}]({prefix}:{name})", alt.trim())
    }
}

/// A file's page, from its type and name.
fn page_for(class: &str, name: &str) -> String {
    format!("/{}:{name}", crate::files::prefix_of(class))
}

/// GET /media/{prefix}/{file}
pub async fn serve(
    State(state): State<AppState>,
    Path((prefix, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    // Only names this engine writes: 64 hex characters and a known extension.
    let Some((stem, _)) = file.rsplit_once('.') else {
        return Ok(crate::errors::not_found());
    };
    let Some(kind) = mime_for(&file) else {
        return Ok(crate::errors::not_found());
    };
    let hex = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if stem.len() != 64 || !hex(stem) {
        return Ok(crate::errors::not_found());
    }
    let key = if prefix == "avatars" || prefix == "emotes" {
        format!("{prefix}/{file}")
    } else if prefix.len() == 2 && hex(&prefix) && stem.starts_with(&prefix) {
        format!("media/{prefix}/{file}")
    } else {
        return Ok(crate::errors::not_found());
    };
    let etag = format!("\"{stem}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let Some(data) = state.storage.get(&key).await? else {
        return Ok(crate::errors::not_found());
    };
    // A player seeks with a byte range; one range is all it asks for.
    let total = data.len();
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| byte_range(v, total));
    let mut response = match range {
        Some((start, end)) => {
            let mut response = Response::new(Body::from(data[start..=end].to_vec()));
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            response
        }
        None => Response::new(Body::from(data)),
    };
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind.mime));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
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
    // A document is saved, never opened in the site's origin.
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static(if kind.class == "document" {
            "attachment"
        } else {
            "inline"
        }),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, value);
    }
    Ok(response)
}

/// The upload page with the uploader's recent files.
async fn render_page(
    state: &AppState,
    ctx: &crate::resolve::Ctx,
    status: StatusCode,
    uploaded: Option<minijinja::Value>,
    error: Option<Refusal>,
) -> Result<Response, AppError> {
    let mine = match ctx.actor.user_id {
        Some(me) => sqlx::query!(
            "SELECT storage_key, filename, name, kind, width, height, created_at FROM media
             WHERE wiki_id = $1 AND uploader_id = $2 ORDER BY created_at DESC LIMIT 24",
            ctx.wiki.id,
            me
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|row| {
            let url = url_for_key(&row.storage_key);
            minijinja::context! {
                url => url.clone(),
                markdown => markdown_for(&row.kind, &row.name, &row.filename),
                page => page_for(&row.kind, &row.name),
                kind => row.kind,
                name => row.name,
                width => row.width,
                height => row.height,
                at => row.created_at.format("%Y-%m-%d").to_string(),
            }
        })
        .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let template = ctx
        .skin
        .env
        .get_template("media.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("media.title"),
                version => ENGINE_VERSION,
                uploaded => uploaded,
                error => error.map(|refusal| refusal_message(state, ctx, refusal)),
                mine => mine,
                max_mb => naw_core::html::mib(state.config.upload_max_bytes),
            }
        })
        .map_err(template_error)?;
    Ok(crate::pages::private_page(status, html))
}

fn refusal_message(state: &AppState, ctx: &crate::resolve::Ctx, refusal: Refusal) -> String {
    let max = naw_core::html::mib(state.config.upload_max_bytes).to_string();
    ctx.t_with(
        &format!("media.refused_{}", refusal.slug()),
        &[("max", &max)],
    )
}

/// A refused upload, as JSON or as the page.
async fn refuse(
    state: &AppState,
    ctx: &crate::resolve::Ctx,
    wants_json: bool,
    refusal: Refusal,
) -> Result<Response, AppError> {
    if wants_json {
        let message = refusal_message(state, ctx, refusal);
        return Ok((refusal.status(), axum::Json(json!({ "error": message }))).into_response());
    }
    render_page(state, ctx, refusal.status(), None, Some(refusal)).await
}

/// GET /media
pub async fn page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    if !ctx.actor.can(Capability::PageEdit) {
        if !ctx.actor.is_signed_in() {
            return Ok(pages::see_other("/login?next=%2Fmedia"));
        }
        return Ok((StatusCode::FORBIDDEN, "uploading needs edit rights").into_response());
    }
    render_page(&state, &ctx, StatusCode::OK, None, None).await
}

/// Records a stored file as this wiki's media, uploaded by the actor, and
/// returns the name of its page. The same bytes uploaded again keep the name
/// they already have; a new file takes its own name, numbered when taken.
async fn record(
    state: &AppState,
    ctx: &crate::resolve::Ctx,
    stored: &Stored,
    filename: &str,
    action: &'static str,
    source: Option<&str>,
) -> Result<String, AppError> {
    let stem = crate::files::stem_of(filename);
    let mut name = None;
    for n in 1..=100 {
        let candidate = crate::files::numbered(&stem, n, stored.kind.ext);
        let inserted = sqlx::query_scalar!(
            "INSERT INTO media (id, wiki_id, uploader_id, storage_key, filename, mime, size_bytes,
                                width, height, name, kind)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT DO NOTHING
             RETURNING name",
            Uuid::new_v4(),
            ctx.wiki.id,
            ctx.actor.user_id,
            stored.key,
            filename,
            stored.kind.mime,
            stored.size as i64,
            stored.width.map(|w| w as i32),
            stored.height.map(|h| h as i32),
            candidate,
            stored.kind.class
        )
        .fetch_optional(&state.db)
        .await?;
        if inserted.is_some() {
            name = inserted;
            break;
        }
        // Either these bytes are here already, or the name is taken.
        if let Some(existing) = sqlx::query_scalar!(
            "SELECT name FROM media WHERE wiki_id = $1 AND storage_key = $2",
            ctx.wiki.id,
            stored.key
        )
        .fetch_optional(&state.db)
        .await?
        {
            return Ok(existing);
        }
    }
    let Some(name) = name else {
        tracing::warn!(%stem, "no free file name after 100 tries");
        return Err(AppError::Internal);
    };
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action,
            entity_type: "media",
            entity_id: None,
            meta: json!({ "key": stored.key, "name": name, "size": stored.size, "mime": stored.kind.mime, "source": source }),
        },
    )
    .await;
    Ok(name)
}

/// A file name for an imported image: the last path segment of its URL.
fn name_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or_default();
    let decoded: String = percent_decode(last);
    clean_filename(&decoded)
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = raw
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Downloads an image from `url` into this wiki's media, as an upload by the
/// actor: the same checks, the same limit and the same daily allowance.
pub async fn import(
    state: &AppState,
    ctx: &crate::resolve::Ctx,
    url: &str,
) -> Result<Result<(Stored, String, String), Refusal>, AppError> {
    if !within_daily_quota(state, ctx).await? {
        return Ok(Err(Refusal::Quota));
    }
    let data = match crate::fetch::get(state, url, state.config.upload_max_bytes).await {
        Ok(fetched) => fetched.bytes,
        Err(crate::fetch::FetchError::Refused) => return Ok(Err(Refusal::Address)),
        Err(crate::fetch::FetchError::TooLarge) => return Ok(Err(Refusal::TooLarge)),
        Err(err) => {
            tracing::debug!(error = %err, "image import failed");
            return Ok(Err(Refusal::Unreachable));
        }
    };
    // A link in an article stands for a picture; anything else is refused.
    if sniff(&data).is_none_or(|kind| kind.class != "image") {
        return Ok(Err(Refusal::NotAnImage));
    }
    let stored = match store(state, "media", data, state.config.upload_max_bytes).await? {
        Ok(stored) => stored,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let filename = name_from_url(url);
    let name = record(state, ctx, &stored, &filename, "media.import", Some(url)).await?;
    Ok(Ok((stored, filename, name)))
}

/// Outside images a save downloads at most, and how many at once.
const LOCALIZE_MAX: usize = 10;
const LOCALIZE_AT_ONCE: usize = 4;
/// Longest a save waits for its images; unfinished ones stay links.
const LOCALIZE_WAIT: std::time::Duration = std::time::Duration::from_secs(25);

/// Replaces outside images in a body with copies stored here, for an actor
/// who may upload. Whatever cannot be downloaded in time stays as written.
pub async fn localize(state: &AppState, ctx: &crate::resolve::Ctx, body: String) -> String {
    if !ctx.actor.can(Capability::PageEdit) {
        return body;
    }
    let found = naw_markdown::external_images(&body);
    if found.is_empty() {
        return body;
    }
    let mut urls: Vec<String> = found.iter().map(|(_, url)| url.clone()).collect();
    urls.sort();
    urls.dedup();
    urls.truncate(LOCALIZE_MAX);
    let mut local: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let work = async {
        for batch in urls.chunks(LOCALIZE_AT_ONCE) {
            let results = futures_util::future::join_all(
                batch
                    .iter()
                    .map(|url| async move { (url.clone(), import(state, ctx, url).await) }),
            )
            .await;
            for (url, result) in results {
                // By page name, so the article links the file the way a person would.
                if let Ok(Ok((_, _, name))) = result {
                    local.insert(url, format!("image:{name}"));
                }
            }
        }
    };
    let _ = tokio::time::timeout(LOCALIZE_WAIT, work).await;
    let mut out = body.clone();
    for (range, url) in found.into_iter().rev() {
        if let Some(replacement) = local.get(&url) {
            out.replace_range(range, replacement);
        }
    }
    out
}

#[derive(serde::Deserialize)]
pub struct ImportForm {
    url: String,
}

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` within `total`, as inclusive offsets.
fn byte_range(raw: &str, total: usize) -> Option<(usize, usize)> {
    let spec = raw.strip_prefix("bytes=")?;
    if spec.contains(',') || total == 0 {
        return None;
    }
    let (from, to) = spec.split_once('-')?;
    let (start, end) = match (from.trim(), to.trim()) {
        ("", n) => {
            let n: usize = n.parse().ok()?;
            (total.checked_sub(n.min(total))?, total - 1)
        }
        (a, "") => (a.parse().ok()?, total - 1),
        (a, b) => (a.parse().ok()?, b.parse::<usize>().ok()?.min(total - 1)),
    };
    (start <= end && start < total).then_some((start, end))
}

/// POST /media/import: an image from a URL, answered like an upload.
pub async fn import_url(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    axum::extract::Form(form): axum::extract::Form<ImportForm>,
) -> Result<Response, AppError> {
    let wants_json = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"));
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    if !ctx.actor.can(Capability::PageEdit) {
        return Ok((StatusCode::FORBIDDEN, "uploading needs edit rights").into_response());
    }
    let (stored, filename, name) = match import(&state, &ctx, &form.url).await? {
        Ok(done) => done,
        Err(refusal) => return refuse(&state, &ctx, wants_json, refusal).await,
    };
    let markdown = markdown_for(stored.kind.class, &name, &filename);
    let page = page_for(stored.kind.class, &name);
    if wants_json {
        return Ok(axum::Json(
            json!({ "url": stored.url, "markdown": markdown, "name": name, "page": page }),
        )
        .into_response());
    }
    let uploaded = minijinja::context! {
        url => stored.url, markdown => markdown, name => name, page => page, kind => stored.kind.class,
    };
    render_page(&state, &ctx, StatusCode::OK, Some(uploaded), None).await
}

/// POST /media/upload: JSON for the editor script, a page for a plain form.
pub async fn upload(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let wants_json = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"));
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    if !ctx.actor.can(Capability::PageEdit) {
        return Ok((StatusCode::FORBIDDEN, "uploading needs edit rights").into_response());
    }
    if !within_daily_quota(&state, &ctx).await? {
        return refuse(&state, &ctx, wants_json, Refusal::Quota).await;
    }
    let (filename, data) =
        match read_file_field(&mut multipart, "file", state.config.upload_max_bytes).await {
            Ok(Some(found)) => found,
            Ok(None) => return refuse(&state, &ctx, wants_json, Refusal::Empty).await,
            Err(refusal) => return refuse(&state, &ctx, wants_json, refusal).await,
        };
    let stored = match store(&state, "media", data, state.config.upload_max_bytes).await? {
        Ok(stored) => stored,
        Err(refusal) => return refuse(&state, &ctx, wants_json, refusal).await,
    };
    let filename = clean_filename(&filename);
    let name = record(&state, &ctx, &stored, &filename, "media.upload", None).await?;
    let markdown = markdown_for(stored.kind.class, &name, &filename);
    let page = page_for(stored.kind.class, &name);
    if wants_json {
        return Ok(axum::Json(json!({
            "url": stored.url,
            "markdown": markdown,
            "name": name,
            "page": page,
            "width": stored.width,
            "height": stored.height,
        }))
        .into_response());
    }
    let uploaded = minijinja::context! {
        url => stored.url, markdown => markdown, name => name, page => page, kind => stored.kind.class,
    };
    render_page(&state, &ctx, StatusCode::OK, Some(uploaded), None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_come_from_the_bytes() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest").map(|k| k.ext), Some("png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]).map(|k| k.ext), Some("jpg"));
        assert_eq!(sniff(b"GIF89a....").map(|k| k.ext), Some("gif"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ").map(|k| k.ext), Some("webp"));
        assert_eq!(sniff(b"\0\0\0\x1cftypavif").map(|k| k.ext), Some("avif"));
        assert_eq!(
            sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"),
            None,
            "never SVG"
        );
        assert_eq!(sniff(b"<html>"), None);
        assert_eq!(sniff(b"ID3\x04rest").map(|k| k.class), Some("audio"));
        assert_eq!(
            sniff(b"OggS\0\x02....OpusHead").map(|k| k.ext),
            Some("opus")
        );
        assert_eq!(sniff(b"\0\0\0\x18ftypmp42").map(|k| k.class), Some("video"));
        assert_eq!(
            sniff(&[0x1A, 0x45, 0xDF, 0xA3, 1]).map(|k| k.ext),
            Some("webm")
        );
        assert_eq!(sniff(b"%PDF-1.7").map(|k| k.class), Some("document"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVEfmt ").map(|k| k.ext), Some("wav"));
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn an_imported_file_is_named_after_its_url() {
        assert_eq!(
            name_from_url("https://x.test/art/Filian%20fan%20art.png?w=2#top"),
            "Filian fan art.png"
        );
        assert_eq!(name_from_url("https://x.test/"), "image");
    }

    #[test]
    fn markdown_uses_the_name_without_its_extension() {
        assert_eq!(
            markdown_for("image", "filian-fan-art.png", "Filian [fan art].png"),
            "![Filian fan art](image:filian-fan-art.png)"
        );
        assert_eq!(
            markdown_for("document", "notes.pdf", "notes.pdf"),
            "[notes](file:notes.pdf)"
        );
        assert_eq!(clean_filename("C:\\Users\\me\\art.png"), "art.png");
        assert_eq!(clean_filename("   "), "image");
    }
}
