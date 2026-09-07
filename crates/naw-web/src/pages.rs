//! Page serving from the render cache. The reader never renders on a hit:
//! a cache miss renders once, stores, and serves.

use axum::body::Body;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use tracing::instrument;
use uuid::Uuid;

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
    summary: &str,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let key = naw_markdown::page_hash(title, locale, &body_md, summary);
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
        &naw_markdown::PageInput {
            title,
            body_md: &body_md,
            wiki_name: &wiki.name,
            lang: locale,
            version: ENGINE_VERSION,
            served_from_cache: false,
            summary,
        },
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

const TITLE_MAX: usize = 200;
const BODY_MAX: usize = 500_000;
const SLUG_MAX: usize = 100;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct EditForm {
    title: String,
    #[serde(default)]
    summary: String,
    body_md: String,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct NewForm {
    slug: String,
    title: String,
    #[serde(default)]
    summary: String,
    body_md: String,
}

/// Slugs are lowercase ASCII, digits and dashes. Anything else is a 422.
/// Unicode slugs come with the i18n pass, not with the launch slice.
fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= SLUG_MAX
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn bad_request(message: &str) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, message.to_string()).into_response()
}

// Nine coherent template bindings for one form render; grouping them
// would hide the mapping. Same reason as the seed orchestrator.
#[allow(clippy::too_many_arguments)]
fn render_form(
    state: &AppState,
    wiki_name: &str,
    lang: &str,
    form_title: &str,
    form_action: &str,
    show_slug: bool,
    slug: &str,
    title_value: &str,
    body_md: &str,
) -> Result<Response, AppError> {
    let template = state
        .templates
        .get_template("edit.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            title => form_title,
            wiki_name => wiki_name,
            lang => lang,
            version => ENGINE_VERSION,
            form_title => form_title,
            form_action => form_action,
            show_slug => show_slug,
            slug => slug,
            title_value => title_value,
            body_md => body_md,
        })
        .map_err(template_error)?;
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct PageQuery {
    #[serde(default)]
    jump_to: Option<String>,
}

struct FoundPage {
    id: Uuid,
    title: String,
    locked: bool,
    body_md: String,
    summary: Option<String>,
}

async fn find_page(
    db: &sqlx::PgPool,
    wiki: &WikiRef,
    slug: &str,
    locale: &str,
) -> Result<Option<FoundPage>, AppError> {
    let page_row = sqlx::query!(
        "SELECT id, title, is_locked, current_revision_id FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2 AND COALESCE(locale, '') = COALESCE($3, '')",
        wiki.id,
        slug,
        locale
    )
    .fetch_optional(db)
    .await?;
    let Some(prow) = page_row else {
        return Ok(None);
    };
    let Some(revision_id) = prow.current_revision_id else {
        return Ok(None);
    };
    let revision = sqlx::query!(
        "SELECT body_md, summary FROM revisions WHERE id = $1",
        revision_id
    )
    .fetch_optional(db)
    .await?;
    Ok(revision.map(|rev| FoundPage {
        id: prow.id,
        title: prow.title,
        locked: prow.is_locked,
        body_md: rev.body_md,
        summary: rev.summary,
    }))
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
    let found = find_page(&state.db, wiki, &slug, &locale).await?;
    let Some(found) = found else {
        return not_found(&state, &locale, &wiki.name).await;
    };
    if let Some(target) = jump_target(&slug, &query) {
        return Ok(Redirect::to(&target).into_response());
    }
    render_or_cached(
        &state,
        wiki,
        &found.title,
        &locale,
        found.body_md,
        found.summary.as_deref().unwrap_or(""),
        &headers,
    )
    .await
}

/// Blank creation form.
#[instrument(skip(state))]
pub async fn new_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let wikis = load_wikis(&state.db).await?;
    let Some(wiki) = resolve_wiki(request_host(&headers), &wikis) else {
        return not_found(&state, "en", "NotAnotherWiki").await;
    };
    render_form(
        &state,
        &wiki.name,
        &wiki.default_locale,
        "New page",
        "/new",
        true,
        "",
        "",
        "",
    )
}

/// Creates the page, its first revision and its cache row, then redirects.
#[instrument(skip(state))]
pub async fn create_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<NewForm>,
) -> Result<Response, AppError> {
    let slug = form.slug.trim().to_lowercase();
    let title = form.title.trim().to_string();
    if !is_valid_slug(&slug) {
        return Ok(bad_request(
            "slug: lowercase letters, digits and dashes, up to 100 chars",
        ));
    }
    if title.is_empty() || title.len() > TITLE_MAX {
        return Ok(bad_request("title: 1 to 200 chars"));
    }
    if form.body_md.is_empty() || form.body_md.len() > BODY_MAX {
        return Ok(bad_request("body: 1 to 500000 chars"));
    }
    let wikis = load_wikis(&state.db).await?;
    let Some(wiki) = resolve_wiki(request_host(&headers), &wikis) else {
        return not_found(&state, "en", "NotAnotherWiki").await;
    };
    let locale = wiki.default_locale.clone();
    let taken = sqlx::query!(
        "SELECT id FROM pages WHERE wiki_id = $1 AND slug = $2",
        wiki.id,
        slug
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();
    if taken {
        return Ok((StatusCode::CONFLICT, "a page with this slug already exists").into_response());
    }
    let summary = form.summary.trim().to_string();
    let summary = if summary.is_empty() {
        None
    } else {
        Some(summary)
    };
    let page_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale) VALUES ($1, $2, 'main', $3, $4, $5)",
        page_id,
        wiki.id,
        slug.clone(),
        title.clone(),
        locale.clone()
    )
    .execute(&state.db)
    .await?;
    sqlx::query!(
        "INSERT INTO revisions (id, page_id, author_id, body_md, content_hash, summary) VALUES ($1, $2, NULL, $3, $4, $5)",
        revision_id,
        page_id,
        form.body_md.clone(),
        naw_markdown::content_hash(&form.body_md),
        summary
    )
    .execute(&state.db)
    .await?;
    sqlx::query!(
        "UPDATE pages SET current_revision_id = $1, updated_at = now() WHERE id = $2",
        revision_id,
        page_id
    )
    .execute(&state.db)
    .await?;
    let rendered = naw_markdown::render_page(
        &state.templates,
        &naw_markdown::PageInput {
            title: &title,
            body_md: &form.body_md,
            wiki_name: &wiki.name,
            lang: &locale,
            version: ENGINE_VERSION,
            served_from_cache: false,
            summary: summary.as_deref().unwrap_or(""),
        },
    )?;
    sqlx::query!(
        "INSERT INTO render_cache (wiki_id, content_hash, renderer_version, html) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        wiki.id,
        rendered.content_hash,
        naw_markdown::RENDERER_VERSION,
        rendered.html
    )
    .execute(&state.db)
    .await?;
    Ok(Redirect::to(&format!("/{slug}")).into_response())
}

/// Edit form prefilled with the current revision.
#[instrument(skip(state))]
pub async fn edit_page(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    let wikis = load_wikis(&state.db).await?;
    let Some(wiki) = resolve_wiki(request_host(&headers), &wikis) else {
        return not_found(&state, "en", "NotAnotherWiki").await;
    };
    let locale = wiki.default_locale.clone();
    let found = find_page(&state.db, wiki, &slug, &locale).await?;
    let Some(found) = found else {
        return not_found(&state, &locale, &wiki.name).await;
    };
    render_form(
        &state,
        &wiki.name,
        &locale,
        &format!("Editing {}", found.title),
        &format!("/{slug}/edit"),
        false,
        &slug,
        &found.title,
        &found.body_md,
    )
}

/// Saves a new revision over an existing page and re-renders it.
#[instrument(skip(state))]
pub async fn save_page(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<EditForm>,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    let title = form.title.trim().to_string();
    if title.is_empty() || title.len() > TITLE_MAX {
        return Ok(bad_request("title: 1 to 200 chars"));
    }
    if form.body_md.is_empty() || form.body_md.len() > BODY_MAX {
        return Ok(bad_request("body: 1 to 500000 chars"));
    }
    let wikis = load_wikis(&state.db).await?;
    let Some(wiki) = resolve_wiki(request_host(&headers), &wikis) else {
        return not_found(&state, "en", "NotAnotherWiki").await;
    };
    let locale = wiki.default_locale.clone();
    let found = find_page(&state.db, wiki, &slug, &locale).await?;
    let Some(found) = found else {
        return not_found(&state, &locale, &wiki.name).await;
    };
    if found.locked {
        return Ok((StatusCode::FORBIDDEN, "this page is locked").into_response());
    }
    let summary = form.summary.trim().to_string();
    let summary = if summary.is_empty() {
        None
    } else {
        Some(summary)
    };
    let revision_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO revisions (id, page_id, author_id, body_md, content_hash, summary) VALUES ($1, $2, NULL, $3, $4, $5)",
        revision_id,
        found.id,
        form.body_md.clone(),
        naw_markdown::content_hash(&form.body_md),
        summary
    )
    .execute(&state.db)
    .await?;
    sqlx::query!(
        "UPDATE pages SET title = $1, current_revision_id = $2, updated_at = now() WHERE id = $3",
        title.clone(),
        revision_id,
        found.id
    )
    .execute(&state.db)
    .await?;
    let rendered = naw_markdown::render_page(
        &state.templates,
        &naw_markdown::PageInput {
            title: &title,
            body_md: &form.body_md,
            wiki_name: &wiki.name,
            lang: &locale,
            version: ENGINE_VERSION,
            served_from_cache: false,
            summary: summary.as_deref().unwrap_or(""),
        },
    )?;
    sqlx::query!(
        "INSERT INTO render_cache (wiki_id, content_hash, renderer_version, html) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        wiki.id,
        rendered.content_hash,
        naw_markdown::RENDERER_VERSION,
        rendered.html
    )
    .execute(&state.db)
    .await?;
    Ok(Redirect::to(&format!("/{slug}")).into_response())
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct PreviewForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body_md: String,
}

/// Renders the posted Markdown without saving anything. The editor opens
/// it in a new tab, so authors see the real pipeline output.
#[instrument(skip(state))]
pub async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PreviewForm>,
) -> Result<Response, AppError> {
    let wikis = load_wikis(&state.db).await?;
    let Some(wiki) = resolve_wiki(request_host(&headers), &wikis) else {
        return not_found(&state, "en", "NotAnotherWiki").await;
    };
    let title = form.title.trim();
    let title = if title.is_empty() { "Preview" } else { title };
    let rendered = naw_markdown::render_page(
        &state.templates,
        &naw_markdown::PageInput {
            title,
            body_md: &form.body_md,
            wiki_name: &wiki.name,
            lang: &wiki.default_locale,
            version: ENGINE_VERSION,
            served_from_cache: false,
            summary: "",
        },
    )?;
    Ok((
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        rendered.html,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::is_valid_slug;

    #[test]
    fn slugs_accept_plain_names() {
        assert!(is_valid_slug("home"));
        assert!(is_valid_slug("filian-lore-2"));
    }

    #[test]
    fn slugs_reject_paths_and_case() {
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("Home"));
        assert!(!is_valid_slug("a/b"));
        assert!(!is_valid_slug("a b"));
        assert!(!is_valid_slug("../home"));
    }
}
