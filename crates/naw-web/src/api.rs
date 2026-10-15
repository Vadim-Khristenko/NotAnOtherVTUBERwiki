//! The public API, `/api/v1`: read-only JSON for bots, widgets and tools.
//!
//! It answers what an anonymous reader of the wiki may see and nothing more:
//! live pages and their accepted revisions. Edits waiting for review, deleted
//! pages and anything about accounts beyond the public name stay out. No
//! session is read, so a call never acts as anyone, and any site may call it
//! (`Access-Control-Allow-Origin: *`). Requests share the per-address rate
//! budgets of the site (`ratelimit.rs`).
//!
//! Lists page with a cursor (`next`), never with offsets that drift.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages;
use crate::resolve::Ctx;

/// Items in one list answer unless `limit` says fewer.
const DEFAULT_LIMIT: i64 = 50;
/// And at most.
const MAX_LIMIT: i64 = 200;

fn limit(asked: Option<i64>) -> i64 {
    asked.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// A JSON answer any site may read, cached briefly by browsers and proxies.
fn ok(value: Value) -> Response {
    let mut res = Json(value).into_response();
    let h = res.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    res
}

/// `{"error": {"code", "message"}}` with its status.
fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    let mut res = (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response();
    let h = res.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn not_found(what: &str) -> Response {
    fail(StatusCode::NOT_FOUND, "not_found", what)
}

/// The wiki the request is for, as an anonymous reader sees it.
async fn context(state: &AppState, headers: &HeaderMap) -> Result<Option<Ctx>, AppError> {
    crate::resolve::context(state, headers, None).await
}

/// The content language asked for, or the one the address picked.
fn locale_of(ctx: &Ctx, asked: Option<&str>) -> Option<String> {
    match asked {
        None => Some(ctx.content_locale.clone()),
        Some(l) if valid_locale(l) => Some(l.to_string()),
        Some(_) => None,
    }
}

fn valid_locale(l: &str) -> bool {
    let (lang, region) = l.split_once('-').unwrap_or((l, ""));
    (2..=3).contains(&lang.len())
        && lang.chars().all(|c| c.is_ascii_lowercase())
        && (region.is_empty()
            || (region.len() == 2 && region.chars().all(|c| c.is_ascii_uppercase())))
}

/// A page's address on the site, absolute.
fn page_url(ctx: &Ctx, locale: &str, path: &str) -> String {
    let link = if locale.is_empty() || locale == ctx.wiki.default_locale {
        format!("/{path}")
    } else {
        ctx.link_for(locale, &format!("/{path}"))
    };
    crate::seo::absolute(ctx, &link)
}

/// Rendered HTML with every relative address made absolute, so it works
/// wherever it is shown.
fn absolutize(ctx: &Ctx, locale: &str, html: &str) -> String {
    let mut out = String::with_capacity(html.len() + 256);
    let mut rest = html;
    loop {
        let next = [" href=\"", " src=\"", " srcset=\""]
            .iter()
            .filter_map(|attr| rest.find(attr).map(|at| (at, *attr)))
            .min_by_key(|(at, _)| *at);
        let Some((at, attr)) = next else {
            out.push_str(rest);
            break;
        };
        let start = at + attr.len();
        out.push_str(&rest[..start]);
        let end = rest[start..].find('"').map_or(rest.len(), |e| start + e);
        let value = &rest[start..end];
        if attr == " srcset=\"" {
            let parts: Vec<String> = value
                .split(',')
                .map(|part| {
                    let part = part.trim();
                    let (url, size) = part.split_once(' ').unwrap_or((part, ""));
                    let url = absolute_ref(ctx, locale, url);
                    if size.is_empty() {
                        url
                    } else {
                        format!("{url} {size}")
                    }
                })
                .collect();
            out.push_str(&parts.join(", "));
        } else {
            out.push_str(&absolute_ref(ctx, locale, value));
        }
        rest = &rest[end..];
    }
    out
}

fn absolute_ref(ctx: &Ctx, locale: &str, value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if value.is_empty()
        || value.starts_with('#')
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("tg:")
        || lower.starts_with("data:")
        || value.starts_with("//")
    {
        return value.to_string();
    }
    if value.starts_with('/') {
        return crate::seo::absolute(ctx, value);
    }
    // a relative page link, as `[[bao]]` renders
    let (path, frag) = value.split_once('#').map_or((value, ""), |(p, f)| (p, f));
    let url = page_url(ctx, locale, path.trim_start_matches("./"));
    if frag.is_empty() {
        url
    } else {
        format!("{url}#{frag}")
    }
}

#[derive(Serialize)]
struct Author {
    name: Option<String>,
    display_name: Option<String>,
    /// For a revision brought over from another wiki: who wrote it there.
    imported_from: Option<String>,
}

fn author(
    username: Option<String>,
    display: Option<String>,
    imported_author: Option<String>,
    imported_from: Option<String>,
) -> Author {
    match imported_author {
        Some(name) => Author {
            name: Some(name),
            display_name: None,
            imported_from,
        },
        None => Author {
            name: username,
            display_name: display,
            imported_from: None,
        },
    }
}

const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

// ---- GET /api: the human page about it -----------------------------------------

pub async fn docs(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    // a real page for the examples: the article edited last
    let sample = sqlx::query_scalar!(
        r#"SELECT slug FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL
             AND current_revision_id IS NOT NULL AND COALESCE(locale, '') = COALESCE($2, '')
           ORDER BY updated_at DESC LIMIT 1"#,
        ctx.wiki.id,
        ctx.content_locale
    )
    .fetch_optional(&state.db)
    .await?
    .unwrap_or_else(|| "home".to_string());
    let title = ctx.t("api.title");
    let html = pages::render_shell(
        &ctx,
        &pages::Shell {
            title: &title,
            body_html: "",
            render_ms: None,
            template: "api.html",
            extra: minijinja::context! {
                api_base => crate::seo::absolute(&ctx, "/api/v1"),
                api_sample => sample,
                api_license => crate::seo::absolute(&ctx, "/license"),
            },
        },
    )?;
    Ok(([HTML], html).into_response())
}

// ---- GET /api/v1 ------------------------------------------------------------

pub async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    let base = crate::seo::absolute(&ctx, "/api/v1");
    Ok(ok(json!({
        "wiki": {
            "name": ctx.wiki.name,
            "url": crate::seo::absolute(&ctx, "/"),
            "default_locale": ctx.wiki.default_locale,
            "license": crate::seo::absolute(&ctx, "/license"),
        },
        "engine": { "name": "NotAnotherWiki", "version": env!("CARGO_PKG_VERSION") },
        "docs": crate::seo::absolute(&ctx, "/api"),
        "openapi": format!("{base}/openapi.json"),
        "endpoints": {
            "pages": format!("{base}/pages"),
            "page": format!("{base}/pages/{{path}}"),
            "history": format!("{base}/pages/{{path}}/history"),
            "revision": format!("{base}/revisions/{{id}}"),
            "search": format!("{base}/search?q={{text}}"),
            "recent": format!("{base}/recent"),
            "category": format!("{base}/categories/{{key}}"),
        },
    })))
}

// ---- GET /api/v1/pages --------------------------------------------------------

#[derive(Deserialize)]
pub struct ListQuery {
    ns: Option<String>,
    locale: Option<String>,
    limit: Option<i64>,
    /// The last path of the previous answer.
    after: Option<String>,
}

const NAMESPACES: [&str; 4] = ["main", "template", "page_template", "module"];

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    let ns = q.ns.as_deref().unwrap_or("main");
    if !NAMESPACES.contains(&ns) {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_namespace",
            "ns is one of main, template, page_template, module",
        ));
    }
    let Some(locale) = locale_of(&ctx, q.locale.as_deref()) else {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_locale",
            "locale looks like en or pt-BR",
        ));
    };
    let n = limit(q.limit);
    let after = q
        .after
        .as_deref()
        .map(|a| pages::split_path(a).1.to_string())
        .unwrap_or_default();
    let rows = sqlx::query!(
        r#"SELECT p.slug, p.title, p.locale, p.updated_at, p.current_revision_id AS "revision!"
           FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace::text = $2
             AND COALESCE(p.locale, '') = COALESCE($3, '')
             AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND p.slug > $4
           ORDER BY p.slug
           LIMIT $5"#,
        ctx.wiki.id,
        ns,
        locale,
        after,
        n + 1
    )
    .fetch_all(&state.db)
    .await?;
    let more = rows.len() as i64 > n;
    let items: Vec<Value> = rows
        .iter()
        .take(n as usize)
        .filter_map(|r| {
            let path = pages::path_of(ns, &r.slug)?;
            let loc = r.locale.clone().unwrap_or_default();
            Some(json!({
                "path": path,
                "title": r.title,
                "locale": r.locale,
                "url": page_url(&ctx, &loc, &path),
                "revision": r.revision,
                "updated_at": r.updated_at,
            }))
        })
        .collect();
    let next = more
        .then(|| {
            items
                .last()
                .and_then(|i| i["path"].as_str().map(str::to_string))
        })
        .flatten();
    Ok(ok(json!({ "items": items, "next": next })))
}

// ---- GET /api/v1/pages/{path} --------------------------------------------------

#[derive(Deserialize)]
pub struct PageQuery {
    locale: Option<String>,
    /// `markdown` (the default), `html` or `both`.
    format: Option<String>,
}

pub async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    if !pages::slug_is_valid(&path) {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_path",
            "a page path looks like bao-the-whale or template:infobox",
        ));
    }
    let Some(locale) = locale_of(&ctx, q.locale.as_deref()) else {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_locale",
            "locale looks like en or pt-BR",
        ));
    };
    let format = q.format.as_deref().unwrap_or("markdown");
    if !matches!(format, "markdown" | "html" | "both") {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_format",
            "format is markdown, html or both",
        ));
    }
    let Some(found) = pages::find_page(&state.db, ctx.wiki.id, &path, &locale).await? else {
        return Ok(not_found("no such page"));
    };
    let rev = sqlx::query!(
        r#"SELECT r.created_at, r.summary, r.is_minor, r.imported_author, r.imported_from,
                  u.username AS "username?", u.display_name AS "display_name?"
           FROM revisions r LEFT JOIN users u ON u.id = r.author_id
           WHERE r.id = $1"#,
        found.revision_id
    )
    .fetch_one(&state.db)
    .await?;
    let (ns, slug) = pages::split_path(&path);
    let translations = sqlx::query!(
        r#"SELECT p.locale, p.title FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace::text = $2 AND p.slug = $3
             AND COALESCE(p.locale, '') <> COALESCE($4, '')
             AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
           ORDER BY p.locale"#,
        ctx.wiki.id,
        ns,
        slug,
        locale
    )
    .fetch_all(&state.db)
    .await?;
    let rendered = pages::cached_body_full(&state, &ctx, &path, &found.body_md).await?;
    let categories: Vec<Value> = rendered
        .categories
        .iter()
        .map(|c| json!({ "key": c.key, "name": c.name, "url": crate::seo::absolute(&ctx, &ctx.link(&format!("/category:{}", c.key))) }))
        .collect();
    let mut body = json!({
        "path": path,
        "namespace": ns,
        "title": found.title,
        "locale": locale,
        "url": page_url(&ctx, &locale, &path),
        "updated_at": found.updated_at,
        "revision": {
            "id": found.revision_id,
            "created_at": rev.created_at,
            "summary": rev.summary,
            "minor": rev.is_minor,
            "author": author(rev.username, rev.display_name, rev.imported_author, rev.imported_from),
        },
        "categories": categories,
        "translations": translations.iter().map(|t| {
            let loc = t.locale.clone().unwrap_or_default();
            json!({ "locale": t.locale, "title": t.title, "url": page_url(&ctx, &loc, &path) })
        }).collect::<Vec<_>>(),
        "license": crate::seo::absolute(&ctx, "/license"),
    });
    if format != "html" {
        body["markdown"] = json!(found.body_md);
    }
    if format != "markdown" {
        body["html"] = json!(absolutize(&ctx, &locale, &rendered.html));
    }
    Ok(ok(body))
}

// ---- GET /api/v1/pages/{path}/history ------------------------------------------

#[derive(Deserialize)]
pub struct HistoryQuery {
    locale: Option<String>,
    limit: Option<i64>,
    /// The `created_at` of the last revision in the previous answer.
    before: Option<DateTime<Utc>>,
}

pub async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    if !pages::slug_is_valid(&path) {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_path",
            "a page path looks like bao-the-whale or template:infobox",
        ));
    }
    let Some(locale) = locale_of(&ctx, q.locale.as_deref()) else {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_locale",
            "locale looks like en or pt-BR",
        ));
    };
    let Some(found) = pages::find_page(&state.db, ctx.wiki.id, &path, &locale).await? else {
        return Ok(not_found("no such page"));
    };
    let n = limit(q.limit);
    let rows = sqlx::query!(
        r#"SELECT r.id, r.created_at, r.summary, r.is_minor, r.bytes AS "bytes!", r.imported_author, r.imported_from,
                  u.username AS "username?", u.display_name AS "display_name?"
           FROM revisions r LEFT JOIN users u ON u.id = r.author_id
           WHERE r.page_id = $1 AND r.review_status = 'accepted'
             AND ($2::timestamptz IS NULL OR r.created_at < $2)
           ORDER BY r.created_at DESC
           LIMIT $3"#,
        found.id,
        q.before,
        n + 1
    )
    .fetch_all(&state.db)
    .await?;
    let more = rows.len() as i64 > n;
    let items: Vec<Value> = rows
        .into_iter()
        .take(n as usize)
        .map(|r| {
            json!({
                "id": r.id,
                "created_at": r.created_at,
                "summary": r.summary,
                "minor": r.is_minor,
                "bytes": r.bytes,
                "current": r.id == found.revision_id,
                "author": author(r.username, r.display_name, r.imported_author, r.imported_from),
            })
        })
        .collect();
    let next = more
        .then(|| items.last().map(|i| i["created_at"].clone()))
        .flatten();
    Ok(ok(
        json!({ "path": path, "locale": locale, "items": items, "next": next }),
    ))
}

// ---- GET /api/v1/revisions/{id} ------------------------------------------------

pub async fn revision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    let Ok(id) = Uuid::parse_str(&id) else {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_id",
            "a revision id is a UUID",
        ));
    };
    let Some(r) = sqlx::query!(
        r#"SELECT r.created_at, r.summary, r.is_minor, r.body_md, r.imported_author, r.imported_from,
                  p.namespace::text AS "namespace!", p.slug, p.title, p.locale, (p.current_revision_id = r.id) AS "current!",
                  u.username AS "username?", u.display_name AS "display_name?"
           FROM revisions r
           JOIN pages p ON p.id = r.page_id
           LEFT JOIN users u ON u.id = r.author_id
           WHERE r.id = $1 AND p.wiki_id = $2 AND p.deleted_at IS NULL AND r.review_status = 'accepted'"#,
        id,
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(not_found("no such revision"));
    };
    let Some(path) = pages::path_of(&r.namespace, &r.slug) else {
        return Ok(not_found("no such revision"));
    };
    let loc = r.locale.clone().unwrap_or_default();
    Ok(ok(json!({
        "id": id,
        "created_at": r.created_at,
        "summary": r.summary,
        "minor": r.is_minor,
        "current": r.current,
        "author": author(r.username, r.display_name, r.imported_author, r.imported_from),
        "page": { "path": path, "title": r.title, "locale": r.locale, "url": page_url(&ctx, &loc, &path) },
        "markdown": r.body_md,
        "license": crate::seo::absolute(&ctx, "/license"),
    })))
}

// ---- GET /api/v1/search ----------------------------------------------------------

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    locale: Option<String>,
    /// `all` searches every language.
    all: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    let text = q.q.as_deref().map(str::trim).unwrap_or_default();
    if text.is_empty() || text.chars().count() > 200 {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_query",
            "q is 1 to 200 characters",
        ));
    }
    let Some(locale) = locale_of(&ctx, q.locale.as_deref()) else {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_locale",
            "locale looks like en or pt-BR",
        ));
    };
    let every = q.all.unwrap_or(false);
    let n = limit(q.limit).min(50);
    let offset = q.offset.unwrap_or(0).clamp(0, 1000);
    let config = naw_core::search::regconfig_for(&locale);
    let like = crate::search::like(text);
    let rows = sqlx::query!(
        r#"WITH q AS (SELECT websearch_to_tsquery($2::text::regconfig, $3) AS tsq)
           SELECT p.namespace::text AS "namespace!", p.slug, p.title, p.locale, p.updated_at,
                  ((lower(p.title) = lower($3))::int * 5
                   + (p.title ILIKE $4 || '%')::int * 2
                   + ts_rank(p.search_vector, q.tsq) * 3
                   + similarity(p.title, $3))::float8 AS "score!",
                  ts_headline($2::text::regconfig, coalesce(c.body, ''), q.tsq,
                    'MaxWords=30, MinWords=12, ShortWord=3, MaxFragments=1, StartSel=' || chr(57344) || ', StopSel=' || chr(57345)
                  ) AS "snippet!"
           FROM pages p
           CROSS JOIN q
           LEFT JOIN search_chunks c ON c.page_id = p.id AND c.chunk_no = 1
           WHERE p.wiki_id = $1 AND p.namespace = 'main'
             AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND ($5 OR COALESCE(p.locale, '') = COALESCE($6, ''))
             AND (p.title ILIKE '%' || $4 || '%' OR p.title % $3 OR p.search_vector @@ q.tsq)
           ORDER BY 6 DESC, p.updated_at DESC
           LIMIT $7 OFFSET $8"#,
        ctx.wiki.id,
        config,
        text,
        like,
        every,
        locale,
        n + 1,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    let more = rows.len() as i64 > n;
    let items: Vec<Value> = rows
        .into_iter()
        .take(n as usize)
        .filter_map(|r| {
            let path = pages::path_of(&r.namespace, &r.slug)?;
            let loc = r.locale.clone().unwrap_or_default();
            Some(json!({
                "path": path,
                "title": r.title,
                "locale": r.locale,
                "url": page_url(&ctx, &loc, &path),
                "updated_at": r.updated_at,
                "score": (r.score * 1000.0).round() / 1000.0,
                // HTML: the text escaped, the matched words in <mark>
                "snippet": naw_core::search::clean_snippet(&r.snippet),
            }))
        })
        .collect();
    Ok(ok(
        json!({ "q": text, "items": items, "next_offset": more.then_some(offset + n) }),
    ))
}

// ---- GET /api/v1/recent ----------------------------------------------------------

#[derive(Deserialize)]
pub struct RecentQuery {
    locale: Option<String>,
    limit: Option<i64>,
    before: Option<DateTime<Utc>>,
}

pub async fn recent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RecentQuery>,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    let locale = match q.locale.as_deref() {
        None => None,
        Some(l) if valid_locale(l) => Some(l.to_string()),
        Some(_) => {
            return Ok(fail(
                StatusCode::BAD_REQUEST,
                "bad_locale",
                "locale looks like en or pt-BR",
            ));
        }
    };
    let n = limit(q.limit);
    let rows = sqlx::query!(
        r#"SELECT r.id, r.created_at, r.summary, r.is_minor, r.bytes AS "bytes!", r.imported_author, r.imported_from,
                  p.namespace::text AS "namespace!", p.slug, p.title, p.locale,
                  u.username AS "username?", u.display_name AS "display_name?"
           FROM revisions r
           JOIN pages p ON p.id = r.page_id
           LEFT JOIN users u ON u.id = r.author_id
           WHERE p.wiki_id = $1 AND p.deleted_at IS NULL AND r.review_status = 'accepted'
             AND ($2::text IS NULL OR COALESCE(p.locale, '') = $2)
             AND ($3::timestamptz IS NULL OR r.created_at < $3)
           ORDER BY r.created_at DESC
           LIMIT $4"#,
        ctx.wiki.id,
        locale,
        q.before,
        n + 1
    )
    .fetch_all(&state.db)
    .await?;
    let more = rows.len() as i64 > n;
    let items: Vec<Value> = rows
        .into_iter()
        .take(n as usize)
        .filter_map(|r| {
            let path = pages::path_of(&r.namespace, &r.slug)?;
            let loc = r.locale.clone().unwrap_or_default();
            Some(json!({
                "revision": r.id,
                "created_at": r.created_at,
                "summary": r.summary,
                "minor": r.is_minor,
                "bytes": r.bytes,
                "author": author(r.username, r.display_name, r.imported_author, r.imported_from),
                "page": { "path": path, "title": r.title, "locale": r.locale, "url": page_url(&ctx, &loc, &path) },
            }))
        })
        .collect();
    let next = more
        .then(|| items.last().map(|i| i["created_at"].clone()))
        .flatten();
    Ok(ok(json!({ "items": items, "next": next })))
}

// ---- GET /api/v1/categories/{key} --------------------------------------------------

#[derive(Deserialize)]
pub struct CategoryQuery {
    limit: Option<i64>,
    /// The last sort key of the previous answer.
    after: Option<String>,
}

pub async fn category(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Query(q): Query<CategoryQuery>,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    let key = key.trim().to_lowercase();
    if key.is_empty() || key.chars().count() > 200 {
        return Ok(fail(
            StatusCode::BAD_REQUEST,
            "bad_key",
            "a category key is its address after category:",
        ));
    }
    let n = limit(q.limit);
    let after = q.after.unwrap_or_default();
    let rows = sqlx::query!(
        r#"SELECT p.namespace::text AS "namespace!", p.slug, p.title, p.locale,
                  COALESCE(pc.sort_key, p.title) AS "sort!", pc.name
           FROM page_categories pc
           JOIN pages p ON p.id = pc.page_id
           WHERE pc.wiki_id = $1 AND pc.category = $2
             AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND COALESCE(pc.sort_key, p.title) > $3
           ORDER BY 5, p.slug
           LIMIT $4"#,
        ctx.wiki.id,
        key,
        after,
        n + 1
    )
    .fetch_all(&state.db)
    .await?;
    if rows.is_empty() && after.is_empty() {
        return Ok(not_found("no pages in this category"));
    }
    let more = rows.len() as i64 > n;
    let name = rows.first().map(|r| r.name.clone());
    let shown: Vec<_> = rows.into_iter().take(n as usize).collect();
    let next = more.then(|| shown.last().map(|r| r.sort.clone())).flatten();
    let items: Vec<Value> = shown
        .into_iter()
        .filter_map(|r| {
            let path = pages::path_of(&r.namespace, &r.slug)?;
            let loc = r.locale.clone().unwrap_or_default();
            Some(json!({ "path": path, "title": r.title, "locale": r.locale, "url": page_url(&ctx, &loc, &path) }))
        })
        .collect();
    Ok(ok(json!({
        "key": key,
        "name": name,
        "url": crate::seo::absolute(&ctx, &ctx.link(&format!("/category:{key}"))),
        "items": items,
        "next": next,
    })))
}

// ---- GET /api/v1/openapi.json -------------------------------------------------------

pub async fn openapi(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(ctx) = context(&state, &headers).await? else {
        return Ok(not_found("no wiki at this address"));
    };
    let p = |name: &str, place: &str, kind: &str, about: &str| json!({ "name": name, "in": place, "required": place == "path", "schema": { "type": kind }, "description": about });
    let get = |summary: &str, params: Vec<Value>| {
        json!({ "get": { "summary": summary, "parameters": params, "responses": {
        "200": { "description": "OK" }, "400": { "description": "A bad parameter: {error: {code, message}}" },
        "404": { "description": "Not found" }, "429": { "description": "Too many requests; see Retry-After" } } } })
    };
    let locale = || {
        p(
            "locale",
            "query",
            "string",
            "Content language, such as en or ru; the address's own by default",
        )
    };
    let lim = || {
        p(
            "limit",
            "query",
            "integer",
            "Items in one answer, 1 to 200, 50 by default",
        )
    };
    Ok(ok(json!({
        "openapi": "3.1.0",
        "info": {
            "title": format!("{} API", ctx.wiki.name),
            "version": "1",
            "description": "Read-only. Pages, history, search and recent changes as an anonymous reader sees them. No key; rate limited per address. Content is under the wiki's license.",
            "license": { "name": "Content license", "url": crate::seo::absolute(&ctx, "/license") },
        },
        "servers": [{ "url": crate::seo::absolute(&ctx, "/api/v1") }],
        "paths": {
            "/": get("The wiki and the endpoints", vec![]),
            "/pages": get("Live pages, by path", vec![
                p("ns", "query", "string", "main (default), template, page_template or module"), locale(), lim(),
                p("after", "query", "string", "The last path of the previous answer")]),
            "/pages/{path}": get("A page: its text, rendered HTML, categories and translations", vec![
                p("path", "path", "string", "bao-the-whale, template:infobox"), locale(),
                p("format", "query", "string", "markdown (default), html or both")]),
            "/pages/{path}/history": get("Accepted revisions, newest first", vec![
                p("path", "path", "string", "A page path"), locale(), lim(),
                p("before", "query", "string", "created_at of the last item of the previous answer (RFC 3339)")]),
            "/revisions/{id}": get("One accepted revision with its text", vec![p("id", "path", "string", "A revision UUID")]),
            "/search": get("Articles whose title or text matches", vec![
                p("q", "query", "string", "The words"), locale(), p("all", "query", "boolean", "Every language"),
                p("limit", "query", "integer", "1 to 50"), p("offset", "query", "integer", "Skip this many")]),
            "/recent": get("Recent accepted edits on the wiki", vec![
                p("locale", "query", "string", "Only this language"), lim(),
                p("before", "query", "string", "created_at of the last item of the previous answer (RFC 3339)")]),
            "/categories/{key}": get("Pages in a category", vec![
                p("key", "path", "string", "The category's address after category:"), lim(),
                p("after", "query", "string", "The last sort key of the previous answer")]),
        },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_are_checked_before_they_reach_a_query() {
        for ok in ["en", "ru", "pt-BR", "yue"] {
            assert!(valid_locale(ok), "{ok}");
        }
        for bad in [
            "", "e", "english", "EN", "en-us", "en_GB", "en-BRA", "ru;drop", "../x",
        ] {
            assert!(!valid_locale(bad), "{bad}");
        }
    }

    #[test]
    fn a_list_limit_stays_in_bounds() {
        assert_eq!(limit(None), DEFAULT_LIMIT);
        assert_eq!(limit(Some(0)), 1);
        assert_eq!(limit(Some(-5)), 1);
        assert_eq!(limit(Some(10_000)), MAX_LIMIT);
        assert_eq!(limit(Some(7)), 7);
    }
}
