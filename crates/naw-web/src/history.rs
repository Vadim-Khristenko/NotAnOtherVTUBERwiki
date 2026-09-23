//! Revision history: the list, one old revision, the diff, and revert.
//!
//! A wiki without history is a website. Every save has been writing a
//! `revisions` row since the first migration, but nothing ever read them back,
//! so the whole record was invisible and `author_id` was never even filled in.
//!
//! Revert is deliberately not a privileged action. It writes an ordinary
//! revision whose body is an old body, which means it is visible in the
//! history, diffable, and revertible in turn. Anybody who may edit the page may
//! do it, exactly as undo works on every wiki that people actually use.

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
use crate::pages::{self, ENGINE_VERSION, find_page};
use crate::perm::Capability;
use crate::resolve::context;

/// Revisions per page of history.
const PER_PAGE: i64 = 50;

/// Unchanged lines kept either side of a change in a diff.
const CONTEXT: usize = 3;

/// Most diff rows we will put in one document. A pair of 500 KB bodies with
/// nothing in common is 20 000 rows of markup, which helps nobody and costs
/// everybody. Past this the diff is truncated and says so.
const DIFF_ROW_MAX: usize = 1500;

/// Longest the diff algorithm may search, on a page anyone can open. Past the
/// deadline the crate stops looking for the shortest diff and returns a
/// correct, coarser one. It needs the crate's `std` feature: without it the
/// deadline type is `()` and the limit silently does nothing.
const DIFF_TIME_MAX: std::time::Duration = std::time::Duration::from_millis(250);

/// Longest body, in lines, whose diff still gets tidied. The crate's
/// compaction pass slides hunks to cleaner boundaries, ignores the deadline,
/// and is quadratic on long repetitive bodies: two 60 000 line bodies of
/// repeating short lines spent 2.5 s there against 75 ms in the diff itself.
/// Past this the rows come straight from the diff.
const COMPACT_LINES_MAX: usize = 2000;

const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

fn slug_or_404(raw: &str) -> Option<String> {
    let slug = raw.trim().to_lowercase();
    pages::slug_is_valid(&slug).then_some(slug)
}

/// One row in the history list.
struct RevisionRow {
    id: Uuid,
    author: Option<String>,
    summary: Option<String>,
    is_minor: bool,
    is_patrolled: bool,
    restored_from: Option<Uuid>,
    bytes: i32,
    created_at: chrono::DateTime<chrono::Utc>,
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
    let Some(ctx) = context(&state, &headers, user.as_ref()).await? else {
        return Ok(crate::errors::not_found());
    };
    let locale = ctx.content_locale.clone();
    let Some(found) = find_page(&state.db, ctx.wiki.id, &slug, &locale).await? else {
        return Ok(crate::errors::not_found());
    };

    // Page numbers come from a query string, so clamp rather than trust. A
    // negative offset is an error in PostgreSQL, not an empty result.
    let page_no = query.page.unwrap_or(1).max(1);
    let offset = (page_no - 1) * PER_PAGE;

    let total = sqlx::query!(
        "SELECT count(*) AS \"count!\" FROM revisions WHERE page_id = $1",
        found.id
    )
    .fetch_one(&state.db)
    .await?
    .count;

    // octet_length rather than the body itself: the history list needs the
    // size of fifty revisions, not fifty megabytes of their text.
    let rows = sqlx::query!(
        r#"
        SELECT r.id, r.summary, r.is_minor, r.is_patrolled, r.created_at,
               r.reverted_revision_id,
               octet_length(r.body_md) AS "bytes!",
               u.username AS "author?"
        FROM revisions r
        LEFT JOIN users u ON u.id = r.author_id
        WHERE r.page_id = $1
        ORDER BY r.created_at DESC, r.id DESC
        LIMIT $2 OFFSET $3
        "#,
        found.id,
        PER_PAGE,
        offset
    )
    .fetch_all(&state.db)
    .await?;

    let revisions: Vec<RevisionRow> = rows
        .into_iter()
        .map(|row| RevisionRow {
            id: row.id,
            author: row.author,
            summary: row.summary,
            is_minor: row.is_minor,
            is_patrolled: row.is_patrolled,
            restored_from: row.reverted_revision_id,
            bytes: row.bytes,
            created_at: row.created_at,
        })
        .collect();

    let may_edit = ctx.actor.can_edit_page(found.protection);
    let items: Vec<minijinja::Value> = revisions
        .iter()
        .map(|rev| {
            minijinja::context! {
                id => rev.id.to_string(),
                author => rev.author.clone(),
                summary => rev.summary.clone(),
                is_minor => rev.is_minor,
                is_patrolled => rev.is_patrolled,
                restored_from => rev.restored_from.map(|id| id.to_string()),
                bytes => rev.bytes,
                created_at => rev.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
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
                total => total,
                page_no => page_no,
                has_prev => page_no > 1,
                has_next => offset + PER_PAGE < total,
                prev_page => page_no - 1,
                next_page => page_no + 1,
                may_edit => may_edit,
                may_patrol => ctx.actor.can(Capability::RevisionPatrol),
                // A page with one revision has nothing earlier to go back to.
                // Without this the restore form rendered with an empty dropdown
                // and a button that could only fail.
                has_restorable => total > 1,
            }
        })
        .map_err(pages::template_error)?;
    Ok((StatusCode::OK, [HTML], html).into_response())
}

/// One stored revision, loaded by id and checked against its page.
struct StoredRevision {
    body_md: String,
    summary: Option<String>,
    author: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Loads a revision, but only if it belongs to `page_id`.
///
/// The page check is the security part: revision ids arrive in the URL, and
/// without it anybody could read a revision of any page on any wiki in the
/// install by guessing or harvesting ids, including from an archived page.
async fn load_revision(
    db: &sqlx::PgPool,
    page_id: Uuid,
    revision_id: Uuid,
) -> Result<Option<StoredRevision>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT r.body_md, r.summary, r.created_at, u.username AS "author?"
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
        body_md: row.body_md,
        summary: row.summary,
        author: row.author,
        created_at: row.created_at,
    }))
}

/// GET /{slug}/rev/{revision}
///
/// Renders an old revision through the same pipeline as the live page, with a
/// banner saying so. No `noindex` games: the banner plus a canonical link to
/// the live page is what search engines want.
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
    let Some(ctx) = context(&state, &headers, user.as_ref()).await? else {
        return Ok(crate::errors::not_found());
    };
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

    let rendered = naw_markdown::render_body(&stored.body_md);
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
                created_at => stored.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                // The editor's note about this change. It belongs in the banner
                // next to who and when, not at the top of the article: rendering
                // it as the lede is what put "Restored the revision from ..."
                // above the front page.
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

/// What one line of a diff is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Context,
    Added,
    Removed,
    /// A run of unchanged lines that was collapsed away.
    Gap,
}

impl RowKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Context => "ctx",
            Self::Added => "add",
            Self::Removed => "del",
            Self::Gap => "gap",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: RowKind,
    /// 1-based line number in the old text, absent for an added line.
    pub old_line: Option<usize>,
    /// 1-based line number in the new text, absent for a removed line.
    pub new_line: Option<usize>,
    pub text: String,
    /// How many unchanged lines a `Gap` row stands in for. The count rather
    /// than a sentence, because the sentence is a translated message and
    /// building it here would hard-code English into the diff.
    pub hidden: Option<usize>,
}

/// The result of diffing two bodies.
#[derive(Debug, PartialEq, Eq)]
pub struct Diff {
    pub rows: Vec<DiffRow>,
    pub added: usize,
    pub removed: usize,
    /// True when `DIFF_ROW_MAX` cut the output short. The template says so,
    /// because a diff that silently stops is worse than no diff.
    pub truncated: bool,
}

/// Line diff with unchanged runs collapsed.
///
/// Hunk grouping is done here rather than through the crate's unified diff
/// writer because the output is a table in a template, not a patch file, and
/// this way the collapsing rule is one testable function.
pub fn diff_bodies(old: &str, new: &str) -> Diff {
    use similar::algorithms::{Capture, diff_deadline};
    use similar::{Algorithm, ChangeTag, capture_diff_deadline};

    // Lines keep their terminator, as the crate's own line diff does, so a
    // last line with and without a newline still differ.
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let deadline = Some(std::time::Instant::now() + DIFF_TIME_MAX);
    let ops = if old_lines.len().max(new_lines.len()) <= COMPACT_LINES_MAX {
        capture_diff_deadline(
            Algorithm::Myers,
            &old_lines,
            0..old_lines.len(),
            &new_lines,
            0..new_lines.len(),
            deadline,
        )
    } else {
        let mut capture = Capture::new();
        let Ok(()) = diff_deadline(
            Algorithm::Myers,
            &mut capture,
            &old_lines,
            0..old_lines.len(),
            &new_lines,
            0..new_lines.len(),
            deadline,
        );
        capture.into_ops()
    };
    let mut all: Vec<DiffRow> = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    for op in &ops {
        for change in op.iter_changes(&old_lines, &new_lines) {
            let kind = match change.tag() {
                ChangeTag::Equal => RowKind::Context,
                ChangeTag::Insert => {
                    added += 1;
                    RowKind::Added
                }
                ChangeTag::Delete => {
                    removed += 1;
                    RowKind::Removed
                }
            };
            all.push(DiffRow {
                kind,
                // similar counts from zero, readers count from one.
                old_line: change.old_index().map(|i| i + 1),
                new_line: change.new_index().map(|i| i + 1),
                text: change.value().trim_end_matches(['\n', '\r']).to_string(),
                hidden: None,
            });
        }
    }

    let rows = collapse(all);
    let truncated = rows.len() > DIFF_ROW_MAX;
    let rows = if truncated {
        rows.into_iter().take(DIFF_ROW_MAX).collect()
    } else {
        rows
    };
    Diff {
        rows,
        added,
        removed,
        truncated,
    }
}

/// Replaces long runs of unchanged lines with one `Gap` row.
///
/// A run is only worth collapsing when it is longer than the context kept at
/// both ends plus the gap row itself. Collapsing a run of exactly that length
/// would replace seven lines with seven lines and a marker, which is worse
/// than leaving it alone.
fn collapse(rows: Vec<DiffRow>) -> Vec<DiffRow> {
    let keep = CONTEXT * 2 + 1;
    let mut out: Vec<DiffRow> = Vec::with_capacity(rows.len());
    let mut index = 0;
    while index < rows.len() {
        if rows[index].kind != RowKind::Context {
            out.push(rows[index].clone());
            index += 1;
            continue;
        }
        let start = index;
        while index < rows.len() && rows[index].kind == RowKind::Context {
            index += 1;
        }
        let run = &rows[start..index];
        if run.len() <= keep {
            out.extend_from_slice(run);
            continue;
        }
        // A run at the very start or end of the file has only one inner edge,
        // so only that edge needs its context kept.
        let at_start = start == 0;
        let at_end = index == rows.len();
        if !at_start {
            out.extend_from_slice(&run[..CONTEXT]);
        }
        let hidden =
            run.len() - if at_start { 0 } else { CONTEXT } - if at_end { 0 } else { CONTEXT };
        out.push(DiffRow {
            kind: RowKind::Gap,
            old_line: None,
            new_line: None,
            text: String::new(),
            hidden: Some(hidden),
        });
        if !at_end {
            out.extend_from_slice(&run[run.len() - CONTEXT..]);
        }
    }
    out
}

#[derive(Debug, serde::Deserialize)]
pub struct DiffQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

/// GET /{slug}/diff?from=&to=
///
/// `to` defaults to the current revision, which makes "what changed since this
/// old version" a one-parameter link from the history list.
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
    let Some(ctx) = context(&state, &headers, user.as_ref()).await? else {
        return Ok(crate::errors::not_found());
    };
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

    let (Some(from), Some(to)) = (
        load_revision(&state.db, found.id, from_id).await?,
        load_revision(&state.db, found.id, to_id).await?,
    ) else {
        return Ok(crate::errors::not_found());
    };

    let computed = diff_bodies(&from.body_md, &to.body_md);
    let rows: Vec<minijinja::Value> = computed
        .rows
        .iter()
        .map(|row| {
            minijinja::context! {
                kind => row.kind.as_str(),
                old_line => row.old_line,
                new_line => row.new_line,
                text => row.text.clone(),
                hidden => row.hidden,
            }
        })
        .collect();

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
                truncated => computed.truncated,
                identical => computed.added == 0 && computed.removed == 0,
                from_id => from_id.to_string(),
                to_id => to_id.to_string(),
                from_author => from.author.clone(),
                to_author => to.author.clone(),
                from_at => from.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                to_at => to.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                may_edit => ctx.actor.can_edit_page(found.protection),
                to_is_current => to_id == found.revision_id,
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

/// POST /{slug}/revert
///
/// Restores an old body as a new revision. The old revision is untouched: the
/// history grows forwards only, so a revert can itself be reverted and the
/// record of what happened survives.
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
    let Some(ctx) = context(&state, &headers, user.as_ref()).await? else {
        return Ok(crate::errors::not_found());
    };
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
    // Reverting to what is already live would add a revision that changes
    // nothing. Send them to the page instead.
    if target.body_md == found.body_md {
        return Ok(pages::see_other(&ctx.link(&format!("/{slug}"))));
    }

    let revision_id = Uuid::new_v4();
    // The history shows this summary, so it follows the reverting editor's
    // language. Stored as written: a revision summary is a historical record
    // and must not change when somebody else reads it in another language.
    let summary = ctx.t_with(
        "history.restore_summary",
        &[(
            "when",
            &target.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        )],
    );
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        // reverted_revision_id records which revision was restored, so the
        // history list can say "restored from" and link straight to it.
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
    // Compare and swap, as a save does: an edit that landed after this
    // request loaded the page must not be erased by a revert racing it.
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

/// POST /{slug}/patrol
///
/// Marks a revision as checked by a moderator. This is the lightweight half of
/// review: the edit is already live, and patrolling records that a trusted
/// person has looked at it, so the next moderator can skip it.
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
    let Some(ctx) = context(&state, &headers, user.as_ref()).await? else {
        return Ok(crate::errors::not_found());
    };
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
    // The page_id in the WHERE clause is the tenancy check: a revision id from
    // the form must belong to the page in the URL, on the wiki for this host.
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
        // Either it is not a revision of this page, or it was already checked.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(diff: &Diff) -> Vec<&'static str> {
        diff.rows.iter().map(|r| r.kind.as_str()).collect()
    }

    #[test]
    fn an_identical_body_has_no_changes() {
        let diff = diff_bodies("a\nb\nc\n", "a\nb\nc\n");
        assert_eq!(diff.added, 0);
        assert_eq!(diff.removed, 0);
        assert!(!diff.truncated);
    }

    #[test]
    fn a_changed_line_is_one_removal_and_one_addition() {
        let diff = diff_bodies("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(diff.added, 1);
        assert_eq!(diff.removed, 1);
        assert_eq!(kinds(&diff), vec!["ctx", "del", "add", "ctx"]);
    }

    #[test]
    fn line_numbers_are_one_based_and_side_specific() {
        let diff = diff_bodies("keep\nold\n", "keep\nnew\n");
        let removed = diff
            .rows
            .iter()
            .find(|r| r.kind == RowKind::Removed)
            .expect("a removal");
        assert_eq!(removed.old_line, Some(2));
        assert_eq!(removed.new_line, None);
        let added = diff
            .rows
            .iter()
            .find(|r| r.kind == RowKind::Added)
            .expect("an addition");
        assert_eq!(added.new_line, Some(2));
        assert_eq!(added.old_line, None);
    }

    #[test]
    fn trailing_newlines_do_not_reach_the_row_text() {
        // The diff is by line and the crate keeps the newline on the value.
        // Leaving it in would put a blank line after every row in the table.
        let diff = diff_bodies("one\r\n", "two\r\n");
        assert!(diff.rows.iter().all(|r| !r.text.contains('\n')));
        assert!(diff.rows.iter().all(|r| !r.text.contains('\r')));
    }

    #[test]
    fn a_long_unchanged_run_collapses_to_one_gap() {
        let old: String = (0..60).map(|i| format!("line {i}\n")).collect();
        let new = old.replace("line 30", "LINE 30");
        let diff = diff_bodies(&old, &new);
        let gaps = diff.rows.iter().filter(|r| r.kind == RowKind::Gap).count();
        // One gap before the change and one after it.
        assert_eq!(gaps, 2);
        // Sixty lines of context became context radius plus markers.
        assert!(diff.rows.len() < 20, "{} rows", diff.rows.len());
        // The gap carries how much it hid as a number, so the template can put
        // it in a translated sentence. Building the sentence here would have
        // hard-coded English into the diff.
        //
        // The arithmetic, because a bare number here would be unfalsifiable.
        // 60 lines, line 30 replaced. The run above the change is lines 0..29,
        // 30 of them, at the start of the file, so no leading context is kept
        // and 3 trailing are: 30 - 3 = 27 hidden. The run below is lines 31..59,
        // 29 of them, at the end, so 3 leading are kept and no trailing:
        // 29 - 3 = 26 hidden.
        let hidden: Vec<Option<usize>> = diff
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Gap)
            .map(|r| r.hidden)
            .collect();
        assert_eq!(hidden, vec![Some(27), Some(26)]);
        // Every unchanged line is either shown or counted in a gap, never lost.
        let shown_context = diff
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Context)
            .count();
        assert_eq!(shown_context + 27 + 26, 59);
        assert!(
            diff.rows
                .iter()
                .filter(|r| r.kind == RowKind::Gap)
                .all(|r| r.text.is_empty())
        );
        // Only gap rows carry a count.
        assert!(
            diff.rows
                .iter()
                .filter(|r| r.kind != RowKind::Gap)
                .all(|r| r.hidden.is_none())
        );
    }

    #[test]
    fn a_short_run_is_left_alone_rather_than_swapped_for_a_marker() {
        // Seven context lines between two changes is exactly the radius on
        // both sides plus one. Collapsing it would print seven rows where
        // seven rows already were, and add a marker on top.
        let old = format!(
            "X\n{}Y\n",
            (0..7).map(|i| format!("c{i}\n")).collect::<String>()
        );
        let new = old.replace("X\n", "x\n").replace("Y\n", "y\n");
        let diff = diff_bodies(&old, &new);
        assert!(!diff.rows.iter().any(|r| r.kind == RowKind::Gap));
    }

    #[test]
    fn a_run_at_the_start_of_the_file_keeps_no_leading_context() {
        let old: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let new = format!("{old}tail\n");
        let diff = diff_bodies(&old, &new);
        // The only change is at the end, so the first row is the gap itself:
        // there is nothing above it to give context to.
        assert_eq!(diff.rows.first().map(|r| r.kind), Some(RowKind::Gap));
        assert_eq!(diff.added, 1);
        assert_eq!(diff.removed, 0);
    }

    #[test]
    fn an_enormous_diff_is_cut_and_admits_it() {
        let old: String = (0..4000).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..4000).map(|i| format!("new {i}\n")).collect();
        let diff = diff_bodies(&old, &new);
        assert!(diff.truncated);
        assert_eq!(diff.rows.len(), DIFF_ROW_MAX);
        // The totals are counted before truncation, so the header still tells
        // the truth about the size of the change.
        assert_eq!(diff.added, 4000);
        assert_eq!(diff.removed, 4000);
    }

    #[test]
    fn a_long_repetitive_diff_stays_cheap() {
        // Two bodies cycling through a few short lines out of step: seconds
        // of hunk compaction in a release build before it was skipped here.
        let old: String = (0..60_000).map(|i| format!("{}\n", i % 3)).collect();
        let new: String = (0..60_000).map(|i| format!("{}\n", (i + 1) % 2)).collect();
        let started = std::time::Instant::now();
        let diff = diff_bodies(&old, &new);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(diff.truncated);
        assert!(diff.added > 0 && diff.removed > 0);
    }

    #[test]
    fn diffing_against_an_empty_body_is_all_additions() {
        let diff = diff_bodies("", "a\nb\n");
        assert_eq!(diff.added, 2);
        assert_eq!(diff.removed, 0);
    }

    #[test]
    fn cyrillic_lines_survive_the_diff_intact() {
        let diff = diff_bodies("Филиан\nснекерс\n", "Филиан\nСнекерс\n");
        let added = diff
            .rows
            .iter()
            .find(|r| r.kind == RowKind::Added)
            .expect("an addition");
        assert_eq!(added.text, "Снекерс");
    }
}
