//! Page serving from the render cache. The reader never renders on a hit:
//! a cache miss renders once, stores, and serves.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use tracing::instrument;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::resolve::{WikiRef, resolve_wiki};

fn template_error(err: minijinja::Error) -> AppError {
    tracing::error!(error = %err, "template error");
    AppError::Internal
}

/// Engine version shown in the footer. Tracks the workspace release.
const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

async fn load_wikis(db: &sqlx::PgPool) -> Result<Vec<WikiRef>, AppError> {
    let rows =
        sqlx::query!(r#"SELECT id, slug, domain, name, default_locale, settings FROM wikis"#)
            .fetch_all(db)
            .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let is_default = row
                .settings
                .get("default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            WikiRef {
                id: row.id,
                slug: row.slug,
                domain: row.domain,
                name: row.name,
                default_locale: row.default_locale,
                settings: row.settings,
                is_default,
            }
        })
        .collect())
}

fn etag_for(hash: &[u8]) -> String {
    format!("\"{}\"", hex::encode(hash))
}

fn cached_response(html: String, etag: &str, headers: &HeaderMap) -> Response {
    let mut response = ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag);
    if fresh {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        *response.body_mut() = Body::empty();
    }
    response
}

async fn not_found(state: &AppState, lang: &str, wiki_name: &str) -> Result<Response, AppError> {
    let template = state
        .templates
        .get_template("404.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! { lang => lang, wiki_name => wiki_name })
        .map_err(template_error)?;
    Ok((
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response())
}

async fn render_or_cached(
    state: &AppState,
    wiki: &WikiRef,
    title: &str,
    locale: &str,
    body_md: String,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let key = naw_markdown::page_hash(title, locale, &body_md);
    let etag = etag_for(&key);
    if let Some(row) = sqlx::query!(
        "SELECT html FROM render_cache WHERE wiki_id = $1 AND content_hash = $2 AND renderer_version = $3",
        wiki.id,
        key,
        naw_markdown::RENDERER_VERSION
    )
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(cached_response(row.html, &etag, headers));
    }
    let rendered = naw_markdown::render_page(
        &state.templates,
        title,
        &body_md,
        &wiki.name,
        locale,
        ENGINE_VERSION,
        false,
    )?;
    let rendered_etag = etag_for(&rendered.content_hash);
    sqlx::query!(
        "INSERT INTO render_cache (wiki_id, content_hash, renderer_version, html) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        wiki.id,
        rendered.content_hash.clone(),
        naw_markdown::RENDERER_VERSION,
        rendered.html.clone()
    )
    .execute(&state.db)
    .await?;
    Ok(cached_response(rendered.html, &rendered_etag, headers))
}

/// Display flags readers can append to any page URL. `?jump_to=` scrolls
/// to a heading anchor through a temporary redirect. Needs no JavaScript.
fn jump_target(slug: &str, query: &PageQuery) -> Option<String> {
    let frag = query.jump_to.as_deref()?;
    if frag.is_empty()
        || frag.len() > 100
        || !frag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(format!("/{slug}#{frag}"))
}

fn request_host(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::HOST)?.to_str().ok()
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct PageQuery {
    #[serde(default)]
    jump_to: Option<String>,
}

/// Redirects `/` to the wiki home page.
#[instrument(skip(state))]
pub async fn home(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    let wikis = load_wikis(&state.db).await?;
    let Some(wiki) = resolve_wiki(request_host(&headers), &wikis) else {
        return not_found(&state, "en", "NotAnotherWiki").await;
    };
    let slug = wiki
        .settings
        .get("home_slug")
        .and_then(|v| v.as_str())
        .unwrap_or("home");
    Ok(Redirect::permanent(&format!("/{slug}")).into_response())
}

/// Serves one page from the render cache. Main namespace only for launch.
#[instrument(skip(state))]
pub async fn page(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    if slug.is_empty() || slug.len() > 200 {
        return not_found(&state, "en", "NotAnotherWiki").await;
    }
    let wikis = load_wikis(&state.db).await?;
    let Some(wiki) = resolve_wiki(request_host(&headers), &wikis) else {
        return not_found(&state, "en", "NotAnotherWiki").await;
    };
    let locale = wiki.default_locale.clone();
    let page_row = sqlx::query!(
        "SELECT id, title, current_revision_id FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2 AND COALESCE(locale, '') = COALESCE($3, '')",
        wiki.id,
        slug,
        locale.clone()
    )
    .fetch_optional(&state.db)
    .await?;
    let Some(prow) = page_row else {
        return not_found(&state, &locale, &wiki.name).await;
    };
    let Some(revision_id) = prow.current_revision_id else {
        return not_found(&state, &locale, &wiki.name).await;
    };
    let revision = sqlx::query!("SELECT body_md FROM revisions WHERE id = $1", revision_id)
        .fetch_optional(&state.db)
        .await?;
    let Some(rev) = revision else {
        return not_found(&state, &locale, &wiki.name).await;
    };
    if let Some(target) = jump_target(&slug, &query) {
        return Ok(Redirect::to(&target).into_response());
    }
    render_or_cached(&state, wiki, &prow.title, &locale, rev.body_md, &headers).await
}
