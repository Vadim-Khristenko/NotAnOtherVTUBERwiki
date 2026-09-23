//! Serving and editing pages.
//!
//! `render_cache` holds the body fragment only, keyed by the revision's
//! content hash. The document around it is assembled per request, since the
//! chrome depends on who is signed in and must never reach a shared cache row.

use axum::body::Body;
use axum::extract::{Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::instrument;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::audit;
use crate::auth::session::CurrentUser;
use crate::perm::Capability;
use crate::resolve::Ctx;

pub(crate) fn template_error(err: minijinja::Error) -> AppError {
    tracing::error!(error = %err, "template error");
    AppError::Internal
}

/// Engine version shown in the footer.
pub(crate) const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

const TITLE_MAX: usize = 200;
const SUMMARY_MAX: usize = 200;
/// Largest article text, in bytes of Markdown. Images are uploaded
/// separately and never count toward it.
pub(crate) const BODY_MAX: usize = 5 * 1024 * 1024;
/// Largest form carrying an article: urlencoding can triple the text.
pub(crate) const TEXT_FORM_MAX: usize = 3 * BODY_MAX + 256 * 1024;
const SLUG_MAX: usize = 100;

const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

/// A page never stored: it carries a form for secrets, or who is signed in.
pub(crate) fn private_page(status: StatusCode, html: String) -> Response {
    (status, [HTML, (header::CACHE_CONTROL, "no-store")], html).into_response()
}

/// The router fallback, so a miss goes through the middleware and comes out
/// as a themed 404.
pub async fn fallback() -> Response {
    crate::errors::not_found()
}

/// Renders `message.html`, the shared "we will not do that" page.
pub(crate) fn notice(
    ctx: &Ctx,
    status: StatusCode,
    heading: &str,
    message: &str,
    back_href: &str,
    back_label: &str,
) -> Result<Response, AppError> {
    let template = ctx
        .skin
        .env
        .get_template("message.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => heading,
                version => ENGINE_VERSION,
                heading => heading,
                tone => "error",
                message => message,
                back_href => back_href,
                back_label => back_label,
            }
        })
        .map_err(template_error)?;
    Ok((status, [HTML], html).into_response())
}

/// A message key from a query string (`?done=saved`) when it is short,
/// lowercase with underscores and starts with one of `prefixes`, so a crafted
/// link cannot choose the wording.
pub(crate) fn message_key<'a>(raw: Option<&'a str>, prefixes: &[&str]) -> Option<&'a str> {
    raw.filter(|key| {
        !key.is_empty()
            && key.len() < 32
            && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            && prefixes.iter().any(|prefix| key.starts_with(prefix))
    })
}

/// Somebody saved between this editor loading the page and saving it.
pub(crate) fn edit_conflict(ctx: &Ctx, slug: &str) -> Result<Response, AppError> {
    notice(
        ctx,
        StatusCode::CONFLICT,
        &ctx.t("error.conflict_title"),
        "This page changed while you were writing. Your text was not saved. \
         Open the page again, compare it with what you wrote, and re-apply your changes.",
        &ctx.link(&format!("/{slug}/history")),
        &ctx.t("error.conflict_link"),
    )
}

/// The slug is in use in this language, live or archived.
pub(crate) fn slug_taken(ctx: &Ctx, slug: &str, archived: bool) -> Result<Response, AppError> {
    let message = ctx.t(if archived {
        "error.taken_archived"
    } else {
        "error.taken_live"
    });
    notice(
        ctx,
        StatusCode::CONFLICT,
        &ctx.t("error.taken_title"),
        &message,
        &ctx.link(&format!("/{slug}")),
        &ctx.t("error.taken_link"),
    )
}

/// Another page holds `target_locale` for this slug.
fn locale_taken(ctx: &Ctx, slug: &str, target_locale: &str) -> Result<Response, AppError> {
    notice(
        ctx,
        StatusCode::CONFLICT,
        &ctx.t("error.taken_title"),
        &ctx.t_with(
            "editor.locale_taken",
            &[(
                "language",
                &crate::translate::native_name(ctx, target_locale),
            )],
        ),
        &ctx.link_for(target_locale, &format!("/{slug}")),
        &ctx.t("error.taken_link"),
    )
}

/// Whether a write lost a race to a unique index; a 409, not a 500.
pub(crate) fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_unique_violation())
}

/// "You may not do this": a guest is sent to sign in, anyone else gets a 403.
fn refuse(
    ctx: &Ctx,
    return_to: &str,
    refused: Capability,
    explanation: &str,
) -> Result<Response, AppError> {
    tracing::debug!(
        capability = refused.as_str(),
        signed_in = ctx.actor.is_signed_in(),
        role = ?ctx.actor.effective_role(),
        wiki = %ctx.wiki.slug,
        "refused"
    );
    if !ctx.actor.is_signed_in() {
        let target = format!("/login?next={}", urlencode(return_to));
        if let Some(response) = redirect_response(StatusCode::SEE_OTHER, &target) {
            return Ok(response);
        }
    }
    notice(
        ctx,
        StatusCode::FORBIDDEN,
        &ctx.t("error.not_allowed"),
        explanation,
        "/",
        &ctx.t("error.back_to_wiki"),
    )
}

/// Percent-encodes one query parameter value.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Lowercase ASCII letters, digits and dashes.
pub(crate) fn slug_is_valid(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= SLUG_MAX
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn bad_request(message: &str) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, message.to_string()).into_response()
}

/// A redirect, or `None` when `target` is not a valid header value (axum
/// would turn that into a 500).
pub(crate) fn redirect_response(status: StatusCode, target: &str) -> Option<Response> {
    let location = axum::http::HeaderValue::from_str(target).ok()?;
    Response::builder()
        .status(status)
        .header(header::LOCATION, location)
        .body(Body::empty())
        .ok()
}

/// What the document shell needs around a rendered body.
///
/// There is no summary field: `revisions.summary` is the edit summary, not
/// article text.
pub(crate) struct Shell<'a> {
    pub title: &'a str,
    pub body_html: &'a str,
    /// `None` when the body came from cache.
    pub render_ms: Option<u64>,
    /// Template-specific bindings, merged last.
    pub extra: minijinja::Value,
    pub template: &'a str,
}

/// Assembles a document from a body fragment and this request's chrome.
pub(crate) fn render_shell(ctx: &Ctx, shell: &Shell<'_>) -> Result<String, AppError> {
    let footer_note = match shell.render_ms {
        Some(ms) => ctx.skin.messages.render(
            &ctx.lang,
            "footer.rendered_in",
            &[("ms".to_string(), ms.to_string())],
        ),
        None => ctx
            .skin
            .messages
            .render(&ctx.lang, "footer.served_from_cache", &[]),
    };
    let template = ctx
        .skin
        .env
        .get_template(shell.template)
        .map_err(template_error)?;
    template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => shell.title,
                body => shell.body_html,
                version => ENGINE_VERSION,
                footer_note => footer_note,
            },
            ..shell.extra.clone()
        })
        .map_err(template_error)
}

/// The rendered body of one revision, from cache when possible, with emotes
/// expanded. Concurrent misses render twice and one insert wins.
pub(crate) async fn cached_body(
    state: &AppState,
    wiki_id: Uuid,
    body_md: &str,
) -> Result<(String, Option<u64>), AppError> {
    let key = naw_markdown::content_hash(body_md);
    if let Some(row) = sqlx::query!(
        "SELECT html FROM render_cache WHERE wiki_id = $1 AND content_hash = $2 AND renderer_version = $3",
        wiki_id,
        key,
        naw_markdown::RENDERER_VERSION
    )
    .fetch_optional(&state.db)
    .await?
    {
        let html = crate::emotes::expand(state, wiki_id, row.html).await?;
        return Ok((html, None));
    }
    let rendered = naw_markdown::render_body(body_md);
    sqlx::query!(
        "INSERT INTO render_cache (wiki_id, content_hash, renderer_version, html)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        wiki_id,
        rendered.content_hash,
        naw_markdown::RENDERER_VERSION,
        rendered.html
    )
    .execute(&state.db)
    .await?;
    let html = crate::emotes::expand(state, wiki_id, rendered.html).await?;
    Ok((html, Some(rendered.render_ms)))
}

/// Caches a just-saved body so the author's redirect is a cache hit.
pub(crate) async fn warm_cache(state: &AppState, wiki_id: Uuid, body_md: &str) {
    let rendered = naw_markdown::render_body(body_md);
    let result = sqlx::query!(
        "INSERT INTO render_cache (wiki_id, content_hash, renderer_version, html)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        wiki_id,
        rendered.content_hash,
        naw_markdown::RENDERER_VERSION,
        rendered.html
    )
    .execute(&state.db)
    .await;
    // A cold cache costs one render later; never fail a committed save for it.
    if let Err(err) = result {
        tracing::warn!(error = %err, "could not warm the render cache");
    }
}

/// Serves a document with an ETag of the finished HTML and handles
/// If-None-Match. `private`, since the document can carry a username.
pub(crate) fn html_response(html: String, headers: &HeaderMap) -> Response {
    use sha2::{Digest, Sha256};
    // The nonce differs per response, so it stays out of the hash; a 304 then
    // goes out without a new policy (see csp.rs).
    let nonce = naw_core::csp::current();
    let digest = if nonce.is_empty() {
        Sha256::digest(html.as_bytes())
    } else {
        Sha256::digest(html.replace(&nonce, "").as_bytes())
    };
    let etag = format!("\"{}\"", hex::encode(digest));
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str());
    let mut response = ([HTML], html).into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, must-revalidate"),
    );
    if fresh {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        *response.body_mut() = Body::empty();
    }
    response
}

/// One page as the handlers need it.
pub(crate) struct FoundPage {
    pub id: Uuid,
    pub title: String,
    pub locked: bool,
    /// The lowest role that may edit, or `None` when unprotected.
    pub protection: Option<crate::perm::WikiRole>,
    pub revision_id: Uuid,
    pub body_md: String,
    pub summary: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// On a translation, the source language and the source revision it matches.
    pub translation_source_locale: Option<String>,
    pub translation_source_revision_id: Option<Uuid>,
}

/// Loads a live page and its current revision; archived pages read as absent.
pub(crate) async fn find_page(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    slug: &str,
    locale: &str,
) -> Result<Option<FoundPage>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT p.id, p.title, p.is_locked, p.edit_level, p.updated_at,
               p.translation_source_locale, p.translation_source_revision_id,
               r.id AS revision_id, r.body_md, r.summary
        FROM pages p
        JOIN revisions r ON r.id = p.current_revision_id
        WHERE p.wiki_id = $1
          AND p.namespace = 'main'
          AND p.slug = $2
          AND COALESCE(p.locale, '') = COALESCE($3, '')
          AND p.deleted_at IS NULL
        "#,
        wiki_id,
        slug,
        locale
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| FoundPage {
        id: row.id,
        title: row.title,
        locked: row.is_locked,
        protection: protection_of(row.is_locked, row.edit_level.as_deref()),
        revision_id: row.revision_id,
        body_md: row.body_md,
        summary: row.summary,
        updated_at: row.updated_at,
        translation_source_locale: row.translation_source_locale,
        translation_source_revision_id: row.translation_source_revision_id,
    }))
}

/// Protection from the two columns. A lock without a level is an old lock,
/// which meant moderators and up.
pub(crate) fn protection_of(locked: bool, level: Option<&str>) -> Option<crate::perm::WikiRole> {
    match level.and_then(crate::perm::WikiRole::parse) {
        Some(level) => Some(level),
        None if locked => Some(crate::perm::WikiRole::Moderator),
        None => None,
    }
}

/// `?jump_to=` as a redirect to the heading anchor.
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

#[derive(Debug, serde::Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    jump_to: Option<String>,
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Redirects `/` to the wiki home page.
#[instrument(skip(state, user))]
pub async fn home(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let slug = ctx
        .wiki
        .settings
        .get("home_slug")
        .and_then(|v| v.as_str())
        .unwrap_or("home");
    // A broken home_slug must not become a 500 through an invalid Location.
    if !slug_is_valid(slug) {
        return Ok(crate::errors::not_found());
    }
    let target = ctx.link(&format!("/{slug}"));
    match redirect_response(StatusCode::PERMANENT_REDIRECT, &target) {
        Some(response) => Ok(response),
        None => Ok(crate::errors::not_found()),
    }
}

/// Serves one page in the main namespace.
#[instrument(skip(state, user))]
pub async fn page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    // Before the query: an embedded NUL in a text parameter is a 500.
    if !slug_is_valid(&slug) {
        return Ok(crate::errors::not_found());
    }
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        // Offer the article in the languages it exists in, or to translate it.
        return Ok(
            match crate::translate::missing(&state, &ctx, &slug).await? {
                Some(response) => response,
                None => crate::errors::not_found(),
            },
        );
    };
    if let Some(target) = jump_target(&slug, &query).map(|t| ctx.link(&t))
        && let Some(response) = redirect_response(StatusCode::SEE_OTHER, &target)
    {
        return Ok(response);
    }
    let (body_html, render_ms) = cached_body(&state, ctx.wiki.id, &found.body_md).await?;
    let versions = crate::translate::versions(&state, &ctx, &slug).await?;
    let stale = crate::translate::staleness(
        &state,
        &ctx,
        &slug,
        found.translation_source_locale.as_deref(),
        found.translation_source_revision_id,
    )
    .await?;
    let reader_version = (ctx.lang != ctx.content_locale)
        .then(|| versions.iter().find(|v| v.locale == ctx.lang))
        .flatten()
        .map(|v| {
            minijinja::context! {
                href => ctx.link_for(&v.locale, &format!("/{slug}")),
                name => crate::translate::native_name(&ctx, &v.locale),
            }
        });
    let translate = crate::translate::translate_offer(&ctx, &slug, &versions)
        .map(|(href, name)| minijinja::context! { href => href, name => name });
    let html = render_shell(
        &ctx,
        &Shell {
            title: &found.title,
            body_html: &body_html,
            render_ms,
            template: "page.html",
            extra: minijinja::context! {
                slug => slug.clone(),
                locked => found.locked,
                updated_at => found.updated_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                can_edit_this => ctx.actor.can_edit_page(found.protection),
                protection => found.protection.map(crate::perm::WikiRole::as_str),
                protect_choices => crate::protect::choices(&ctx, found.protection),
                other_languages => crate::translate::others(&ctx, &slug, &versions),
                reader_version => reader_version,
                translate => translate,
                stale => stale.map(|s| minijinja::context! {
                    source_name => crate::translate::native_name(&ctx, &s.source_locale),
                    source_href => s.source_href,
                    diff_href => s.diff_href,
                }),
            },
        },
    )?;
    Ok(html_response(html, &headers))
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct EditForm {
    title: String,
    #[serde(default)]
    summary: String,
    body_md: String,
    /// The revision the editor started from; empty from an old skin.
    #[serde(default)]
    base_revision: String,
    #[serde(default)]
    minor: Option<String>,
    /// On a translation: it now matches the source as it is today.
    #[serde(default)]
    synced: Option<String>,
    /// Moves this version to another language when that slot is free.
    #[serde(default)]
    locale: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct NewForm {
    slug: String,
    title: String,
    #[serde(default)]
    summary: String,
    body_md: String,
    /// The article's language, from the editor.
    #[serde(default)]
    locale: String,
}

/// The requested language when this wiki offers it, the reader's otherwise.
fn chosen_locale(ctx: &Ctx, raw: &str) -> String {
    let raw = raw.trim().to_ascii_lowercase();
    if !raw.is_empty() && ctx.offered_languages().contains(&raw) {
        raw
    } else {
        ctx.content_locale.clone()
    }
}

/// Validated form values, shared by create and save.
pub(crate) struct Draft {
    pub title: String,
    pub summary: Option<String>,
    pub body_md: String,
}

/// The reason is plain text; the caller picks the status.
pub(crate) fn validate(title: &str, summary: &str, body_md: &str) -> Result<Draft, &'static str> {
    let title = title.trim().to_string();
    if title.is_empty() || title.chars().count() > TITLE_MAX {
        return Err("title: 1 to 200 characters");
    }
    let summary = summary.trim().to_string();
    if summary.chars().count() > SUMMARY_MAX {
        return Err("summary: up to 200 characters");
    }
    if body_md.is_empty() || body_md.len() > BODY_MAX {
        return Err(
            "body: from 1 byte to 5 MB of text. Images do not count: each is uploaded on its own",
        );
    }
    Ok(Draft {
        title,
        summary: (!summary.is_empty()).then_some(summary),
        body_md: body_md.to_string(),
    })
}

/// The editor form.
pub(crate) struct FormView<'a> {
    pub heading: &'a str,
    pub action: &'a str,
    pub show_slug: bool,
    pub slug: &'a str,
    pub title_value: &'a str,
    pub summary_value: &'a str,
    pub body_md: &'a str,
    pub base_revision: &'a str,
    pub locked: bool,
    /// The title is fixed (a profile), so it travels as a hidden field.
    pub fixed_title: bool,
    /// Where "back" goes when the page is not addressed by `slug`.
    pub back_href: Option<&'a str>,
    /// On a translation, the source language's name.
    pub translation_of: Option<&'a str>,
    /// Presets the language field; `None` hides it.
    pub form_locale: Option<&'a str>,
}

pub(crate) fn render_form(ctx: &Ctx, view: &FormView<'_>) -> Result<Response, AppError> {
    let template = ctx
        .skin
        .env
        .get_template("edit.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => view.heading,
                version => ENGINE_VERSION,
                form_title => view.heading,
                form_action => view.action,
                show_slug => view.show_slug,
                slug => view.slug,
                title_value => view.title_value,
                summary_value => view.summary_value,
                body_md => view.body_md,
                base_revision => view.base_revision,
                locked => view.locked,
                fixed_title => view.fixed_title,
                back_href => view.back_href,
                translation_of => view.translation_of,
                form_locale => view.form_locale,
                body_max => BODY_MAX,
                body_max_mb => naw_core::html::mib(BODY_MAX),
                upload_max => ctx.upload_max_bytes,
                upload_max_mb => naw_core::html::mib(ctx.upload_max_bytes),
                locale_options => view.form_locale.map(|_| {
                    ctx.offered_languages()
                        .into_iter()
                        .map(|code| minijinja::context! {
                            name => crate::translate::native_name(ctx, &code),
                            code => code,
                        })
                        .collect::<Vec<_>>()
                }),
            }
        })
        .map_err(template_error)?;
    Ok(([HTML], html).into_response())
}

/// Blank creation form; needs `PageCreate`.
#[instrument(skip(state, user))]
pub async fn new_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    if !ctx.actor.can(Capability::PageCreate) {
        return refuse(
            &ctx,
            &ctx.link("/new"),
            Capability::PageCreate,
            &ctx.t("error.no_create"),
        );
    }
    render_form(
        &ctx,
        &FormView {
            heading: &ctx.t("editor.new_page"),
            action: &ctx.link("/new"),
            form_locale: Some(&ctx.content_locale),
            show_slug: true,
            slug: "",
            title_value: "",
            summary_value: "",
            body_md: "",
            base_revision: "",
            locked: false,
            fixed_title: false,
            back_href: None,
            translation_of: None,
        },
    )
}

/// Creates the page, its first revision and its search index entry.
#[instrument(skip(state, user))]
pub async fn create_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<NewForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    if !ctx.actor.can(Capability::PageCreate) {
        return refuse(
            &ctx,
            &ctx.link("/new"),
            Capability::PageCreate,
            &ctx.t("error.no_create"),
        );
    }
    let slug = form.slug.trim().to_lowercase();
    if !slug_is_valid(&slug) {
        return Ok(bad_request(
            "slug: lowercase letters, digits and dashes, up to 100 characters",
        ));
    }
    let draft = match validate(&form.title, &form.summary, &form.body_md) {
        Ok(draft) => draft,
        Err(reason) => return Ok(bad_request(reason)),
    };
    let locale = chosen_locale(&ctx, &form.locale);

    // The unique index is the real guard; this answers the common case, and an
    // archived slug stays taken so a restore lands on its own address.
    let taken = sqlx::query!(
        "SELECT deleted_at FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2
           AND COALESCE(locale, '') = $3",
        ctx.wiki.id,
        slug,
        locale
    )
    .fetch_optional(&state.db)
    .await?;
    if let Some(row) = taken {
        return slug_taken(&ctx, &slug, row.deleted_at.is_some());
    }

    let page_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    match sqlx::query!(
        "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale)
         VALUES ($1, $2, 'main', $3, $4, $5)",
        page_id,
        ctx.wiki.id,
        slug,
        draft.title,
        locale
    )
    .execute(&mut *tx)
    .await
    {
        Err(err) if is_unique_violation(&err) => return slug_taken(&ctx, &slug, false),
        result => result?,
    };
    sqlx::query!(
        "INSERT INTO revisions (id, page_id, author_id, body_md, content_hash, summary)
         VALUES ($1, $2, $3, $4, $5, $6)",
        revision_id,
        page_id,
        ctx.actor.user_id,
        draft.body_md,
        naw_markdown::content_hash(&draft.body_md),
        draft.summary
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE pages SET current_revision_id = $1, updated_at = now() WHERE id = $2",
        revision_id,
        page_id
    )
    .execute(&mut *tx)
    .await?;
    naw_core::search::index_page(
        &mut tx,
        page_id,
        &locale,
        &draft.title,
        draft.summary.as_deref(),
        &draft.body_md,
    )
    .await?;
    tx.commit().await?;

    warm_cache(&state, ctx.wiki.id, &draft.body_md).await;
    audit::record_or_log(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "page.create",
            entity_type: "page",
            entity_id: Some(page_id),
            meta: json!({ "slug": slug, "revision": revision_id }),
        },
    )
    .await;
    Ok(see_other(&ctx.link_for(&locale, &format!("/{slug}"))))
}

/// 303 to an internal path, falling back to the wiki root.
pub(crate) fn see_other(target: &str) -> Response {
    redirect_response(StatusCode::SEE_OTHER, target)
        .unwrap_or_else(|| axum::response::Redirect::to("/").into_response())
}

/// Edit form prefilled with the current revision.
#[instrument(skip(state, user))]
pub async fn edit_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    if !slug_is_valid(&slug) {
        return Ok(crate::errors::not_found());
    }
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        return Ok(crate::errors::not_found());
    };
    if !ctx.actor.can_edit_page(found.protection) {
        let explanation = ctx.t(if found.locked {
            "error.page_locked"
        } else {
            "error.no_edit"
        });
        return refuse(
            &ctx,
            &ctx.link(&format!("/{slug}/edit")),
            Capability::PageEdit,
            &explanation,
        );
    }
    let source_name = found
        .translation_source_locale
        .as_deref()
        .map(|l| crate::translate::native_name(&ctx, l));
    render_form(
        &ctx,
        &FormView {
            heading: &ctx.t_with("editor.editing", &[("page", &found.title)]),
            action: &ctx.link(&format!("/{slug}/edit")),
            show_slug: false,
            slug: &slug,
            title_value: &found.title,
            summary_value: found.summary.as_deref().unwrap_or(""),
            body_md: &found.body_md,
            base_revision: &found.revision_id.to_string(),
            locked: found.locked,
            fixed_title: false,
            back_href: None,
            translation_of: source_name.as_deref(),
            form_locale: Some(&ctx.content_locale),
        },
    )
}

/// Saves a new revision over an existing page.
#[instrument(skip(state, user))]
pub async fn save_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<EditForm>,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    if !slug_is_valid(&slug) {
        return Ok(crate::errors::not_found());
    }
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        return Ok(crate::errors::not_found());
    };
    if !ctx.actor.can_edit_page(found.protection) {
        let explanation = ctx.t(if found.locked {
            "error.page_locked"
        } else {
            "error.no_edit"
        });
        return refuse(
            &ctx,
            &ctx.link(&format!("/{slug}/edit")),
            Capability::PageEdit,
            &explanation,
        );
    }
    let draft = match validate(&form.title, &form.summary, &form.body_md) {
        Ok(draft) => draft,
        Err(reason) => return Ok(bad_request(reason)),
    };

    // The editor loaded an older revision: somebody saved in between.
    if let Some(base) = parse_uuid(&form.base_revision)
        && base != found.revision_id
    {
        return edit_conflict(&ctx, &slug);
    }

    // Before the no-change check: a move without a text edit is still a change.
    let target_locale = chosen_locale(&ctx, &form.locale);
    let moved = target_locale != locale;
    if moved {
        let taken = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM pages
               WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2
                 AND COALESCE(locale, '') = $3"#,
            ctx.wiki.id,
            slug,
            target_locale
        )
        .fetch_one(&state.db)
        .await?;
        if taken > 0 {
            return locale_taken(&ctx, &slug, &target_locale);
        }
        let mut tx = state.db.begin().await?;
        // Only from the revision this request loaded, like the text below.
        let relocated = match sqlx::query!(
            "UPDATE pages SET locale = $2, updated_at = now()
             WHERE id = $1 AND current_revision_id = $3",
            found.id,
            target_locale,
            found.revision_id
        )
        .execute(&mut *tx)
        .await
        {
            Err(err) if is_unique_violation(&err) => {
                return locale_taken(&ctx, &slug, &target_locale);
            }
            result => result?,
        };
        if relocated.rows_affected() == 0 {
            return edit_conflict(&ctx, &slug);
        }
        naw_core::search::index_page(
            &mut tx,
            found.id,
            &target_locale,
            &found.title,
            found.summary.as_deref(),
            &found.body_md,
        )
        .await?;
        tx.commit().await?;
        audit::record_or_log(
            &state.db,
            audit::Entry {
                wiki_id: Some(ctx.wiki.id),
                user_id: ctx.actor.user_id,
                action: "page.move_locale",
                entity_type: "page",
                entity_id: Some(found.id),
                meta: json!({ "slug": slug, "from": locale, "to": target_locale }),
            },
        )
        .await;
    }
    let locale = target_locale;

    // Before the no-change check: confirming needs no text edit.
    if form.synced.is_some()
        && let Some(source_locale) = found.translation_source_locale.as_deref()
    {
        sqlx::query!(
            "UPDATE pages SET translation_source_revision_id = (
                 SELECT current_revision_id FROM pages
                 WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2
                   AND COALESCE(locale, '') = $3 AND deleted_at IS NULL)
             WHERE id = $4",
            ctx.wiki.id,
            slug,
            source_locale,
            found.id
        )
        .execute(&state.db)
        .await?;
    }

    // Saving unchanged text adds no revision.
    if draft.body_md == found.body_md
        && draft.title == found.title
        && draft.summary.as_deref().unwrap_or("") == found.summary.as_deref().unwrap_or("")
    {
        return Ok(see_other(&ctx.link_for(&locale, &format!("/{slug}"))));
    }

    let revision_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "INSERT INTO revisions (id, page_id, author_id, body_md, content_hash, summary, is_minor)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        revision_id,
        found.id,
        ctx.actor.user_id,
        draft.body_md,
        naw_markdown::content_hash(&draft.body_md),
        draft.summary,
        form.minor.is_some()
    )
    .execute(&mut *tx)
    .await?;
    // Compare and swap: catches saves racing past the check above.
    let swapped = sqlx::query!(
        "UPDATE pages SET title = $1, current_revision_id = $2, updated_at = now()
         WHERE id = $3 AND current_revision_id = $4",
        draft.title,
        revision_id,
        found.id,
        found.revision_id
    )
    .execute(&mut *tx)
    .await?;
    if swapped.rows_affected() == 0 {
        return edit_conflict(&ctx, &slug);
    }
    naw_core::search::index_page(
        &mut tx,
        found.id,
        &locale,
        &draft.title,
        draft.summary.as_deref(),
        &draft.body_md,
    )
    .await?;
    tx.commit().await?;

    warm_cache(&state, ctx.wiki.id, &draft.body_md).await;
    audit::record_or_log(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "page.edit",
            entity_type: "page",
            entity_id: Some(found.id),
            meta: json!({
                "slug": slug,
                "revision": revision_id,
                "minor": form.minor.is_some(),
            }),
        },
    )
    .await;
    Ok(see_other(&ctx.link_for(&locale, &format!("/{slug}"))))
}

/// A form field UUID; empty or malformed is `None`.
pub(crate) fn parse_uuid(raw: &str) -> Option<Uuid> {
    raw.trim().parse::<Uuid>().ok()
}

// ---------------------------------------------------------------------------
// Language choice
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct LangForm {
    lang: String,
    #[serde(default)]
    next: String,
}

/// `next` when it is a path on this site. Rejects `//host` and backslashes,
/// which browsers read as leaving the site.
fn local_path(next: &str) -> Option<&str> {
    if !next.starts_with('/') || next.starts_with("//") || next.starts_with("/\\") {
        return None;
    }
    if next.contains('\\') || next.contains('\n') || next.contains('\r') {
        return None;
    }
    Some(next)
}

/// The path of the Referer, validated by [`local_path`], as the return
/// address of the language picker. Without a referrer, the front page.
fn path_from_referer(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::REFERER)?.to_str().ok()?;
    let after_scheme = raw.split_once("://").map(|(_, rest)| rest).unwrap_or(raw);
    let path = match after_scheme.find('/') {
        Some(at) => &after_scheme[at..],
        None => "/",
    };
    local_path(path).map(str::to_string)
}

/// POST /lang, the form equivalent of `?lang=`. Both set the cookie through
/// `lang::cookie_for`.
#[instrument(skip(state, user))]
pub async fn set_language(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<LangForm>,
) -> Result<Response, AppError> {
    let referred = path_from_referer(&headers);
    let target = local_path(&form.next)
        .or(referred.as_deref())
        .unwrap_or("/");
    let mut response = see_other(target);
    // An unknown language would be stored and then ignored.
    if state.skin.current().messages.has(&form.lang) {
        let cookie = crate::lang::cookie_for(&form.lang);
        if let Ok(value) = axum::http::HeaderValue::from_str(&cookie) {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
    }
    let _ = user;
    Ok(response)
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct PreviewForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body_md: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct PreviewQuery {
    #[serde(default)]
    fragment: Option<u8>,
}

/// Renders posted Markdown without saving; `?fragment=1` returns only the
/// body for the live preview. Needs `PageEdit`: it is the one uncached
/// render path.
#[instrument(skip(state, user))]
pub async fn preview(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<PreviewQuery>,
    Form(form): Form<PreviewForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    if !ctx.actor.can(Capability::PageEdit) && !ctx.actor.can(Capability::PageCreate) {
        return Ok((StatusCode::FORBIDDEN, "preview needs edit rights").into_response());
    }
    if form.title.chars().count() > TITLE_MAX {
        return Ok(bad_request("title: 1 to 200 characters"));
    }
    if form.body_md.len() > BODY_MAX {
        return Ok((StatusCode::PAYLOAD_TOO_LARGE, "body: up to 5 MB of text").into_response());
    }
    if query.fragment.unwrap_or(0) == 1 {
        let body_html = naw_markdown::render_html(&form.body_md);
        let body_html = crate::emotes::expand(&state, ctx.wiki.id, body_html).await?;
        return Ok(([HTML], body_html).into_response());
    }
    let title = form.title.trim();
    let title = if title.is_empty() { "Preview" } else { title };
    let rendered = naw_markdown::render_body(&form.body_md);
    let body_html = crate::emotes::expand(&state, ctx.wiki.id, rendered.html).await?;
    let html = render_shell(
        &ctx,
        &Shell {
            title,
            body_html: &body_html,
            render_ms: Some(rendered.render_ms),
            template: "page.html",
            extra: minijinja::context! { preview => true },
        },
    )?;
    Ok(([HTML], html).into_response())
}

// ---------------------------------------------------------------------------
// Skin brand files
// ---------------------------------------------------------------------------

/// Brand files served from `{skin_dir}/favicon/`. The fixed list is the whole
/// surface, so no request value reaches the filesystem.
const SKIN_ASSETS: &[(&str, &str)] = &[
    ("favicon.ico", "image/x-icon"),
    ("favicon-96x96.png", "image/png"),
    ("apple-touch-icon.png", "image/png"),
    ("site.webmanifest", "application/manifest+json"),
    ("web-app-manifest-192x192.png", "image/png"),
    ("web-app-manifest-512x512.png", "image/png"),
];

async fn skin_asset(state: &AppState, file: &str) -> Result<Response, AppError> {
    let Some((_, content_type)) = SKIN_ASSETS.iter().find(|(name, _)| *name == file) else {
        return Ok(crate::errors::not_found());
    };
    let path = format!("{}/favicon/{file}", state.config.skin_dir);
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(crate::errors::not_found());
    };
    Ok((
        [
            (header::CONTENT_TYPE, *content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
        .into_response())
}

pub async fn favicon_ico(State(state): State<AppState>) -> Result<Response, AppError> {
    skin_asset(&state, "favicon.ico").await
}

pub async fn favicon_png(State(state): State<AppState>) -> Result<Response, AppError> {
    skin_asset(&state, "favicon-96x96.png").await
}

pub async fn apple_touch_icon(State(state): State<AppState>) -> Result<Response, AppError> {
    skin_asset(&state, "apple-touch-icon.png").await
}

pub async fn site_manifest(State(state): State<AppState>) -> Result<Response, AppError> {
    skin_asset(&state, "site.webmanifest").await
}

pub async fn manifest_icon_192(State(state): State<AppState>) -> Result<Response, AppError> {
    skin_asset(&state, "web-app-manifest-192x192.png").await
}

pub async fn manifest_icon_512(State(state): State<AppState>) -> Result<Response, AppError> {
    skin_asset(&state, "web-app-manifest-512x512.png").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_accept_plain_names() {
        assert!(slug_is_valid("home"));
        assert!(slug_is_valid("filian-lore-2"));
    }

    #[test]
    fn slugs_reject_paths_and_case() {
        assert!(!slug_is_valid(""));
        assert!(!slug_is_valid("Home"));
        assert!(!slug_is_valid("a/b"));
        assert!(!slug_is_valid("a b"));
        assert!(!slug_is_valid("../home"));
        assert!(!slug_is_valid(&"x".repeat(101)));
    }

    fn query(jump_to: Option<&str>) -> PageQuery {
        PageQuery {
            jump_to: jump_to.map(str::to_string),
        }
    }

    #[test]
    fn jump_accepts_plain_anchors() {
        assert_eq!(
            jump_target("home", &query(Some("lore"))),
            Some("/home#lore".to_string())
        );
    }

    #[test]
    fn jump_rejects_markup_and_overflow() {
        assert_eq!(jump_target("home", &query(Some("<script>"))), None);
        assert_eq!(jump_target("home", &query(Some("a b"))), None);
        assert_eq!(jump_target("home", &query(Some("a/b"))), None);
        assert_eq!(jump_target("home", &query(None)), None);
        assert_eq!(jump_target("home", &query(Some(""))), None);
        assert_eq!(jump_target("home", &query(Some(&"x".repeat(101)))), None);
    }

    #[test]
    fn redirects_keep_their_status_and_reject_bad_targets() {
        let ok =
            redirect_response(StatusCode::SEE_OTHER, "/home#lore").expect("plain target is fine");
        assert_eq!(ok.status(), StatusCode::SEE_OTHER);
        assert!(redirect_response(StatusCode::SEE_OTHER, "/ho\nme").is_none());
    }

    #[test]
    fn a_return_path_survives_the_login_round_trip_intact() {
        assert_eq!(urlencode("/filian/edit"), "/filian/edit");
        assert_eq!(urlencode("/a?b=c&d"), "/a%3Fb%3Dc%26d");
        assert_eq!(urlencode("/a b"), "/a%20b");
        assert_eq!(urlencode("//evil.example"), "//evil.example");
    }

    #[test]
    fn validation_counts_characters_not_bytes() {
        // 200 Cyrillic characters are 400 bytes; the limit counts characters.
        let cyrillic = "я".repeat(TITLE_MAX);
        assert!(validate(&cyrillic, "", "body").is_ok());
        assert!(validate(&"я".repeat(TITLE_MAX + 1), "", "body").is_err());
    }

    #[test]
    fn an_empty_title_or_body_is_refused() {
        assert!(validate("", "", "body").is_err());
        assert!(validate("   ", "", "body").is_err());
        assert!(validate("Title", "", "").is_err());
    }

    #[test]
    fn a_blank_summary_becomes_null_rather_than_an_empty_string() {
        assert_eq!(validate("T", "", "b").expect("valid").summary, None);
        assert_eq!(validate("T", "   ", "b").expect("valid").summary, None);
        assert_eq!(
            validate("T", " note ", "b").expect("valid").summary,
            Some("note".to_string())
        );
    }

    #[test]
    fn a_language_switch_can_only_return_somewhere_on_this_site() {
        assert_eq!(local_path("/home"), Some("/home"));
        assert_eq!(
            local_path("/home/history?page=2"),
            Some("/home/history?page=2")
        );
        assert_eq!(local_path("//evil.example"), None);
        assert_eq!(local_path("/\\evil.example"), None);
        assert_eq!(local_path("https://evil.example"), None);
        assert_eq!(local_path("http://evil.example"), None);
        assert_eq!(local_path("evil.example"), None);
        assert_eq!(local_path(""), None);
        assert_eq!(local_path("/home\nLocation: /elsewhere"), None);
        assert_eq!(local_path("/home\r\nSet-Cookie: x=1"), None);
        assert_eq!(local_path("/a\\b"), None);
    }

    #[test]
    fn a_missing_base_revision_does_not_look_like_a_conflict() {
        assert_eq!(parse_uuid(""), None);
        assert_eq!(parse_uuid("   "), None);
        assert_eq!(parse_uuid("not-a-uuid"), None);
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string()), Some(id));
        assert_eq!(parse_uuid(&format!("  {id}  ")), Some(id));
    }
}
