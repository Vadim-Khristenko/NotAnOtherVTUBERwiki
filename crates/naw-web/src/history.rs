//! Revision history: the list, one old revision, the diff, and revert.
//!
//! Revert is not privileged: it writes an ordinary revision with an old body,
//! so it shows in the history and can itself be reverted.

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
use crate::diff::{self, Row, Side};
use crate::pages::{self, ENGINE_VERSION, find_page};
use crate::perm::Capability;

/// Revisions per page of history.
const PER_PAGE: i64 = 50;

/// An edit this many bytes or more either way is shown in bold, as a hint
/// that it is worth a look.
const BIG_EDIT_BYTES: i32 = 500;

/// Jump links above a diff; a diff with more runs of changes lists the first ones.
const HUNK_LINKS_MAX: usize = 30;

const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

fn slug_or_404(raw: &str) -> Option<String> {
    let slug = raw.trim().to_lowercase();
    pages::slug_is_valid(&slug).then_some(slug)
}

fn time_of(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%H:%M").to_string()
}

fn stamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M UTC").to_string()
}

#[derive(Debug, serde::Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    page: Option<i64>,
}

/// GET /{slug}/history
#[instrument(skip(state, user))]
pub async fn history(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    Query(query): Query<HistoryQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(slug) = slug_or_404(&slug) else {
        return Ok(crate::errors::not_found());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        return Ok(crate::errors::not_found());
    };

    // A negative offset is an error in PostgreSQL.
    let page_no = query.page.unwrap_or(1).max(1);
    let offset = (page_no - 1) * PER_PAGE;

    let total = sqlx::query!(
        "SELECT count(*) AS \"count!\" FROM revisions WHERE page_id = $1",
        found.id
    )
    .fetch_one(&state.db)
    .await?
    .count;

    // The window runs over the whole history before the page is cut, so the
    // oldest row on a page still knows the revision before it.
    let rows = sqlx::query!(
        r#"
        SELECT r.id, r.summary, r.is_minor, r.is_patrolled, r.created_at,
               r.reverted_revision_id, r.bytes AS "bytes!",
               r.prev_bytes, r.prev_id,
               u.username AS "author?"
        FROM (
          SELECT id, author_id, summary, is_minor, is_patrolled, created_at,
                 reverted_revision_id, bytes,
                 lag(bytes) OVER w AS prev_bytes,
                 lag(id) OVER w AS prev_id
          FROM revisions
          WHERE page_id = $1
          WINDOW w AS (ORDER BY created_at, id)
        ) r
        LEFT JOIN users u ON u.id = r.author_id
        ORDER BY r.created_at DESC, r.id DESC
        LIMIT $2 OFFSET $3
        "#,
        found.id,
        PER_PAGE,
        offset
    )
    .fetch_all(&state.db)
    .await?;

    let may_edit = ctx.actor.can_edit_page(found.protection);
    let items: Vec<minijinja::Value> = rows
        .iter()
        .map(|rev| {
            let delta = rev.prev_bytes.map(|before| rev.bytes - before);
            minijinja::context! {
                id => rev.id.to_string(),
                author => rev.author.clone(),
                summary => rev.summary.clone(),
                is_minor => rev.is_minor,
                is_patrolled => rev.is_patrolled,
                restored_from => rev.reverted_revision_id.map(|id| id.to_string()),
                bytes => rev.bytes,
                // The first revision's delta is its whole size.
                delta => delta.unwrap_or(rev.bytes),
                delta_big => delta.unwrap_or(rev.bytes).abs() >= BIG_EDIT_BYTES,
                prev_id => rev.prev_id.map(|id| id.to_string()),
                day => ctx.day(rev.created_at),
                time => time_of(rev.created_at),
                created_at => stamp(rev.created_at),
                is_current => rev.id == found.revision_id,
            }
        })
        .collect();

    let template = ctx
        .skin
        .env
        .get_template("history.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t_with("history.title", &[("page", &found.title)]),
                version => ENGINE_VERSION,
                page_title => found.title.clone(),
                slug => slug.clone(),
                locked => found.locked,
                revisions => items,
                current_id => found.revision_id.to_string(),
                total => total,
                page_no => page_no,
                has_prev => page_no > 1,
                has_next => offset + PER_PAGE < total,
                prev_page => page_no - 1,
                next_page => page_no + 1,
                may_edit => may_edit,
                may_patrol => ctx.actor.can(Capability::RevisionPatrol),
            }
        })
        .map_err(pages::template_error)?;
    Ok((StatusCode::OK, [HTML], html).into_response())
}

/// One stored revision, checked against its page.
struct StoredRevision {
    id: Uuid,
    body_md: String,
    summary: Option<String>,
    author: Option<String>,
    is_minor: bool,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Loads a revision only if it belongs to `page_id`, so an id from the URL
/// cannot read another page's history.
async fn load_revision(
    db: &sqlx::PgPool,
    page_id: Uuid,
    revision_id: Uuid,
) -> Result<Option<StoredRevision>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT r.id AS "id!", r.body_md AS "body_md!", r.summary, r.is_minor AS "is_minor!",
               r.created_at AS "created_at!", u.username AS "author?"
        FROM revisions r
        LEFT JOIN users u ON u.id = r.author_id
        WHERE r.id = $1 AND r.page_id = $2
        "#,
        revision_id,
        page_id
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| StoredRevision {
        id: row.id,
        body_md: row.body_md,
        summary: row.summary,
        author: row.author,
        is_minor: row.is_minor,
        created_at: row.created_at,
    }))
}

/// The revision just before or just after `rev` in the page's history.
async fn neighbour(
    db: &sqlx::PgPool,
    page_id: Uuid,
    rev: &StoredRevision,
    older: bool,
) -> Result<Option<Uuid>, AppError> {
    let id = if older {
        sqlx::query_scalar!(
            "SELECT id FROM revisions WHERE page_id = $1 AND (created_at, id) < ($2, $3)
             ORDER BY created_at DESC, id DESC LIMIT 1",
            page_id,
            rev.created_at,
            rev.id
        )
        .fetch_optional(db)
        .await?
    } else {
        sqlx::query_scalar!(
            "SELECT id FROM revisions WHERE page_id = $1 AND (created_at, id) > ($2, $3)
             ORDER BY created_at, id LIMIT 1",
            page_id,
            rev.created_at,
            rev.id
        )
        .fetch_optional(db)
        .await?
    };
    Ok(id)
}

/// GET /{slug}/rev/{revision}: an old revision through the live pipeline,
/// with a banner and a canonical link to the live page.
#[instrument(skip(state, user))]
pub async fn revision(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path((slug, revision_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(slug) = slug_or_404(&slug) else {
        return Ok(crate::errors::not_found());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        return Ok(crate::errors::not_found());
    };
    let Some(revision_id) = pages::parse_uuid(&revision_id) else {
        return Ok(crate::errors::not_found());
    };
    let Some(stored) = load_revision(&state.db, found.id, revision_id).await? else {
        return Ok(crate::errors::not_found());
    };

    let expanded = crate::templates::expand(&state, &ctx, &slug, &stored.body_md).await?;
    let rendered = naw_markdown::render_body(&expanded.text);
    let body_html = crate::emotes::expand(&state, ctx.wiki.id, rendered.html).await?;
    let html = pages::render_shell(
        &ctx,
        &pages::Shell {
            title: &ctx.t_with("revision.suffix", &[("page", &found.title)]),
            body_html: &body_html,
            render_ms: Some(rendered.render_ms),
            template: "revision.html",
            extra: minijinja::context! {
                slug => slug.clone(),
                page_title => found.title.clone(),
                revision_id => revision_id.to_string(),
                author => stored.author.clone(),
                created_at => stamp(stored.created_at),
                edit_summary => stored.summary.clone(),
                is_current => revision_id == found.revision_id,
                may_edit => ctx.actor.can_edit_page(found.protection),
            },
        },
    )?;
    Ok((StatusCode::OK, [HTML], html).into_response())
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

fn side_value(side: &Option<Side>) -> Option<minijinja::Value> {
    side.as_ref().map(|side| {
        let segments: Vec<minijinja::Value> = side
            .segments
            .iter()
            .map(|s| minijinja::context! { text => s.text.clone(), changed => s.changed })
            .collect();
        minijinja::context! { line => side.line, segments => segments }
    })
}

fn row_value(row: &Row) -> minijinja::Value {
    match row {
        Row::Context {
            old_line,
            new_line,
            text,
        } => minijinja::context! {
            kind => row.kind(), old_line => old_line, new_line => new_line, text => text.clone(),
        },
        Row::Change { old, new, hunk } => minijinja::context! {
            kind => row.kind(), hunk => hunk, old => side_value(old), new => side_value(new),
        },
        Row::Gap { hidden } => minijinja::context! { kind => row.kind(), hidden => hidden },
    }
}

fn revision_head(rev: &StoredRevision) -> minijinja::Value {
    minijinja::context! {
        id => rev.id.to_string(),
        author => rev.author.clone(),
        summary => rev.summary.clone(),
        is_minor => rev.is_minor,
        at => stamp(rev.created_at),
        bytes => rev.body_md.len(),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct DiffQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

/// GET /{slug}/diff?from=&to=; `to` defaults to the current revision.
#[instrument(skip(state, user))]
pub async fn diff(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    Query(query): Query<DiffQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(slug) = slug_or_404(&slug) else {
        return Ok(crate::errors::not_found());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        return Ok(crate::errors::not_found());
    };

    let Some(from_id) = query.from.as_deref().and_then(pages::parse_uuid) else {
        return Ok(pages::see_other(&ctx.link(&format!("/{slug}/history"))));
    };
    let to_id = query
        .to
        .as_deref()
        .and_then(pages::parse_uuid)
        .unwrap_or(found.revision_id);

    let (Some(mut from), Some(mut to)) = (
        load_revision(&state.db, found.id, from_id).await?,
        load_revision(&state.db, found.id, to_id).await?,
    ) else {
        return Ok(crate::errors::not_found());
    };
    // Picked the wrong way round in the history: old on the left regardless.
    if (from.created_at, from.id) > (to.created_at, to.id) {
        std::mem::swap(&mut from, &mut to);
    }

    let computed = diff::diff_bodies(&from.body_md, &to.body_md);
    let rows: Vec<minijinja::Value> = computed.rows.iter().map(row_value).collect();
    let hunk_links: Vec<usize> = (1..=computed.hunks.min(HUNK_LINKS_MAX)).collect();
    let older = neighbour(&state.db, found.id, &from, true).await?;
    let newer = neighbour(&state.db, found.id, &to, false).await?;
    let delta = to.body_md.len() as i64 - from.body_md.len() as i64;

    let template = ctx
        .skin
        .env
        .get_template("diff.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t_with("diff.title", &[("page", &found.title)]),
                version => ENGINE_VERSION,
                page_title => found.title.clone(),
                slug => slug.clone(),
                rows => rows,
                added => computed.added,
                removed => computed.removed,
                hunks => computed.hunks,
                hunk_links => hunk_links,
                truncated => computed.truncated,
                identical => computed.added == 0 && computed.removed == 0,
                from => revision_head(&from),
                to => revision_head(&to),
                delta => delta,
                // Stepping one edit back pairs the older neighbour with `from`.
                older_edit => older.map(|id| format!("from={id}&to={}", from.id)),
                newer_edit => newer.map(|id| format!("from={}&to={id}", to.id)),
                may_edit => ctx.actor.can_edit_page(found.protection),
                to_is_current => to.id == found.revision_id,
            }
        })
        .map_err(pages::template_error)?;
    Ok((StatusCode::OK, [HTML], html).into_response())
}

// ---------------------------------------------------------------------------
// Revert
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct RevertForm {
    revision: String,
}

/// POST /{slug}/revert: restores an old body as a new revision.
#[instrument(skip(state, user))]
pub async fn revert(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<RevertForm>,
) -> Result<Response, AppError> {
    let Some(slug) = slug_or_404(&slug) else {
        return Ok(crate::errors::not_found());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        return Ok(crate::errors::not_found());
    };
    if !ctx.actor.can_edit_page(found.protection) {
        return Ok((StatusCode::FORBIDDEN, "reverting needs edit rights").into_response());
    }
    let Some(target_id) = pages::parse_uuid(&form.revision) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "revision: expected an id").into_response());
    };
    let Some(target) = load_revision(&state.db, found.id, target_id).await? else {
        return Ok(crate::errors::not_found());
    };
    // Reverting to the live body would change nothing.
    if target.body_md == found.body_md {
        return Ok(pages::see_other(&ctx.link(&format!("/{slug}"))));
    }

    let revision_id = Uuid::new_v4();
    // Written in the reverting editor's language and kept as written.
    let summary = ctx.t_with(
        "history.restore_summary",
        &[(
            "when",
            &target.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        )],
    );
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "INSERT INTO revisions
           (id, page_id, author_id, body_md, content_hash, summary, reverted_revision_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        revision_id,
        found.id,
        ctx.actor.user_id,
        target.body_md,
        naw_markdown::content_hash(&target.body_md),
        summary,
        target_id
    )
    .execute(&mut *tx)
    .await?;
    // Compare and swap, as a save does.
    let swapped = sqlx::query!(
        "UPDATE pages SET current_revision_id = $1, updated_at = now()
         WHERE id = $2 AND current_revision_id = $3",
        revision_id,
        found.id,
        found.revision_id
    )
    .execute(&mut *tx)
    .await?;
    if swapped.rows_affected() == 0 {
        return pages::edit_conflict(&ctx, &slug);
    }
    naw_core::search::index_page(
        &mut tx,
        found.id,
        &locale,
        &found.title,
        Some(&summary),
        &target.body_md,
    )
    .await?;
    tx.commit().await?;
    pages::after_save(&state, &ctx, found.id, &slug, &target.body_md).await;

    audit::record_or_log(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "page.revert",
            entity_type: "page",
            entity_id: Some(found.id),
            meta: json!({
                "slug": slug,
                "revision": revision_id,
                "restored_from": target_id,
                "replaced": found.revision_id,
            }),
        },
    )
    .await;
    Ok(pages::see_other(&ctx.link(&format!("/{slug}"))))
}

#[derive(Debug, serde::Deserialize)]
pub struct PatrolForm {
    revision: String,
}

/// POST /{slug}/patrol: marks a live revision as checked.
#[instrument(skip(state, user))]
pub async fn patrol(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<PatrolForm>,
) -> Result<Response, AppError> {
    let Some(slug) = slug_or_404(&slug) else {
        return Ok(crate::errors::not_found());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    if !ctx.actor.can(Capability::RevisionPatrol) {
        return Ok((StatusCode::FORBIDDEN, "patrolling needs moderator rights").into_response());
    }
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        return Ok(crate::errors::not_found());
    };
    let Some(revision_id) = pages::parse_uuid(&form.revision) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "revision: expected an id").into_response());
    };
    // `page_id` is the tenancy check for the posted revision id.
    let updated = sqlx::query!(
        "UPDATE revisions SET is_patrolled = true
         WHERE id = $1 AND page_id = $2 AND NOT is_patrolled",
        revision_id,
        found.id
    )
    .execute(&state.db)
    .await?
    .rows_affected();
    if updated == 0 {
        // Not a revision of this page, or already checked.
        return Ok(pages::see_other(&ctx.link(&format!("/{slug}/history"))));
    }
    audit::record_or_log(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "revision.patrol",
            entity_type: "revision",
            entity_id: Some(revision_id),
            meta: json!({ "slug": slug }),
        },
    )
    .await;
    Ok(pages::see_other(&ctx.link(&format!("/{slug}/history"))))
}
