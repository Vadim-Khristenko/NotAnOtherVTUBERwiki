//! Uploaded images: storing, serving, and the upload page.
//!
//! What a file is gets decided by its first bytes, never by its name or the
//! type the browser claims: PNG, JPEG, GIF, WebP and AVIF, and nothing else.
//! SVG is refused outright, because an SVG is a document that can carry
//! script. Files are addressed by the SHA-256 of their content, so the same
//! image uploaded twice is stored once and its URL never changes meaning,
//! which is what lets it be cached forever.
//!
//! Served from `/media/{prefix}/{file}` with `nosniff`, a CSP that allows
//! nothing, and a year of caching. Thumbnails and WebP conversion wait for
//! the worker; until then an image is served as uploaded.

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

/// An image type this engine accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kind {
    pub mime: &'static str,
    pub ext: &'static str,
}

/// Identifies an image by its magic bytes. `None` for anything else.
pub fn sniff(data: &[u8]) -> Option<Kind> {
    let kind = |mime, ext| Some(Kind { mime, ext });
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return kind("image/png", "png");
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return kind("image/jpeg", "jpg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return kind("image/gif", "gif");
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return kind("image/webp", "webp");
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" && matches!(&data[8..12], b"avif" | b"avis") {
        return kind("image/avif", "avif");
    }
    None
}

/// The type to serve for a stored file, from its extension. Only the
/// extensions `sniff` hands out exist in storage.
fn mime_for(file: &str) -> Option<&'static str> {
    let ext = file.rsplit_once('.')?.1;
    Some(match ext {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        _ => return None,
    })
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
    /// The header says image but the dimensions are absurd.
    BadDimensions,
}

impl Refusal {
    pub fn key(self) -> &'static str {
        match self {
            Self::Empty => "media.refused_empty",
            Self::TooLarge => "media.refused_too_large",
            Self::NotAnImage => "media.refused_type",
            Self::BadDimensions => "media.refused_dimensions",
        }
    }
}

/// The address a storage key is served from. Keys are `media/ab/hash.ext`
/// and `avatars/hash.ext`, and both live under `/media/`.
pub fn url_for_key(key: &str) -> String {
    match key.strip_prefix("media/") {
        Some(rest) => format!("/media/{rest}"),
        None => format!("/media/{key}"),
    }
}

/// Checks and stores an image under `prefix` ("media" or "avatars").
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
    let Some(kind) = sniff(&data) else {
        return Ok(Err(Refusal::NotAnImage));
    };
    let size = imagesize::blob_size(&data).ok();
    // A 1 x 1 tracking pixel is still an image; a 100000 pixel wide banner is
    // a way to make every reader's browser allocate gigabytes.
    if let Some(s) = size
        && (s.width == 0 || s.height == 0 || s.width > 16384 || s.height > 16384)
    {
        return Ok(Err(Refusal::BadDimensions));
    }
    let hash = hex::encode(Sha256::digest(&data));
    let (key, url) = if prefix == "media" {
        let key = format!("media/{}/{hash}.{}", &hash[..2], kind.ext);
        let url = format!("/media/{}/{hash}.{}", &hash[..2], kind.ext);
        (key, url)
    } else {
        (
            format!("{prefix}/{hash}.{}", kind.ext),
            format!("/media/{prefix}/{hash}.{}", kind.ext),
        )
    };
    let len = data.len();
    if state.storage.get(&key).await?.is_none() {
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

/// Reads the single file field from a multipart upload, stopping as soon as it
/// goes past `max`, so an oversized upload is never held in memory whole.
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

/// A file name worth keeping for the media list: the base name, trimmed of
/// anything that is not a plain character, and short.
fn clean_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(120).collect();
    if cleaned.trim().is_empty() {
        "image".to_string()
    } else {
        cleaned.trim().to_string()
    }
}

/// The Markdown to paste for an image.
fn markdown_for(url: &str, name: &str) -> String {
    let alt: String = name
        .rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '(' | ')'))
        .collect();
    format!("![{}]({url})", alt.trim())
}

/// GET /media/{prefix}/{file}
pub async fn serve(
    State(state): State<AppState>,
    Path((prefix, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    // Only names this engine writes: 64 hex characters and a known extension,
    // under a two-character hex shard, the avatars or the emotes directory.
    let Some((stem, _)) = file.rsplit_once('.') else {
        return Ok(crate::errors::not_found());
    };
    let Some(mime) = mime_for(&file) else {
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
    let mut response = Response::new(Body::from(data));
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    // An image, displayed and nothing else: no script, no frames, no guessing.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, value);
    }
    Ok(response)
}

/// The upload page: a form, and your recent uploads with their Markdown.
async fn render_page(
    state: &AppState,
    ctx: &crate::resolve::Ctx,
    status: StatusCode,
    uploaded: Option<minijinja::Value>,
    error: Option<&str>,
) -> Result<Response, AppError> {
    let mine = match ctx.actor.user_id {
        Some(me) => sqlx::query!(
            "SELECT storage_key, filename, width, height, created_at FROM media
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
                markdown => markdown_for(&url, &row.filename),
                name => row.filename,
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
                error => error.map(|key| ctx.t_with(key, &[("max", &(state.config.upload_max_bytes / 1024 / 1024).to_string())])),
                mine => mine,
                max_mb => state.config.upload_max_bytes / 1024 / 1024,
            }
        })
        .map_err(template_error)?;
    Ok((
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response())
}

/// Answers a refused upload in the shape the caller asked for.
async fn refuse(
    state: &AppState,
    ctx: &crate::resolve::Ctx,
    wants_json: bool,
    refusal: Refusal,
) -> Result<Response, AppError> {
    let status = if refusal == Refusal::TooLarge {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    if wants_json {
        let max = (state.config.upload_max_bytes / 1024 / 1024).to_string();
        let message = ctx.t_with(refusal.key(), &[("max", &max)]);
        return Ok((status, axum::Json(json!({ "error": message }))).into_response());
    }
    render_page(state, ctx, status, None, Some(refusal.key())).await
}

/// GET /media
pub async fn page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(ctx) = crate::resolve::context(&state, &headers, user.as_ref()).await? else {
        return Ok(crate::errors::not_found());
    };
    if !ctx.actor.can(Capability::PageEdit) {
        if !ctx.actor.is_signed_in() {
            return Ok(pages::see_other("/login?next=%2Fmedia"));
        }
        return Ok((StatusCode::FORBIDDEN, "uploading needs edit rights").into_response());
    }
    render_page(&state, &ctx, StatusCode::OK, None, None).await
}

/// POST /media/upload. Answers JSON to the editor's script (`Accept:
/// application/json`) and a page to a plain form.
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
    let Some(ctx) = crate::resolve::context(&state, &headers, user.as_ref()).await? else {
        return Ok(crate::errors::not_found());
    };
    if !ctx.actor.can(Capability::PageEdit) {
        return Ok((StatusCode::FORBIDDEN, "uploading needs edit rights").into_response());
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
    sqlx::query!(
        "INSERT INTO media (id, wiki_id, uploader_id, storage_key, filename, mime, size_bytes, width, height)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (wiki_id, storage_key) DO NOTHING",
        Uuid::new_v4(),
        ctx.wiki.id,
        ctx.actor.user_id,
        stored.key,
        filename,
        stored.kind.mime,
        stored.size as i64,
        stored.width.map(|w| w as i32),
        stored.height.map(|h| h as i32)
    )
    .execute(&state.db)
    .await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "media.upload",
            entity_type: "media",
            entity_id: None,
            meta: json!({ "key": stored.key, "size": stored.size, "mime": stored.kind.mime }),
        },
    )
    .await;
    let markdown = markdown_for(&stored.url, &filename);
    if wants_json {
        return Ok(axum::Json(json!({
            "url": stored.url,
            "markdown": markdown,
            "width": stored.width,
            "height": stored.height,
        }))
        .into_response());
    }
    let uploaded =
        minijinja::context! { url => stored.url, markdown => markdown, name => filename };
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
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn markdown_uses_the_name_without_its_extension() {
        assert_eq!(
            markdown_for("/media/ab/x.png", "Filian [fan art].png"),
            "![Filian fan art](/media/ab/x.png)"
        );
        assert_eq!(clean_filename("C:\\Users\\me\\art.png"), "art.png");
        assert_eq!(clean_filename("   "), "image");
    }
}
