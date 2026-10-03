//! Edits that wait for review, as with MediaWiki's pending changes, and the
//! curators' queue.
//!
//! A wiki may hold back new pages, edits, or both (`settings.review`). What
//! someone without the pass (`Capability::EditUnreviewed`) publishes there is
//! stored as a pending revision: readers keep the last accepted text, and the
//! pending one shows only to the reviewers and its author. A curator opens it
//! from `/admin/review`, sees the text and what it changes, and accepts it,
//! which makes it the page as a save would, or rejects it with a note.
//!
//! An edit is accepted only over the revision it was written on: once the
//! page has moved on, accepting it would throw away the edit in between.
//!
//! The queue also lists recent edits nobody has checked yet, and a page's
//! history has a rollback: every edit of the last author at once.

use axum::extract::{Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages;
use crate::perm::Capability;
use crate::resolve::Ctx;

/// Rows on one screen of either tab of the queue.
const QUEUE_SHOWN: i64 = 200;

/// Days of unchecked edits the queue lists.
const PATROL_DAYS: i32 = 30;

/// Stores an edit that waits for review over `base`, the revision it was
/// written on (`None` for a new page).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_pending(
    conn: &mut sqlx::PgConnection,
    page_id: Uuid,
    base: Option<Uuid>,
    author: Option<Uuid>,
    title: &str,
    summary: Option<&str>,
    body_md: &str,
    minor: bool,
) -> Result<Uuid, AppError> {
    let id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO revisions (id, page_id, author_id, body_md, content_hash, summary, is_minor,
                                review_status, base_revision_id, review_title)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending', $8, $9)",
        id,
        page_id,
        author,
        body_md,
        naw_markdown::content_hash(body_md),
        summary,
        minor,
        base,
        title
    )
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Whether this reader may see a revision that is not accepted: a reviewer,
/// or its author.
pub(crate) fn may_see(ctx: &Ctx, status: &str, author: Option<Uuid>) -> bool {
    status == "accepted"
        || ctx.actor.can(Capability::RevisionPatrol)
        || (author.is_some() && author == ctx.actor.user_id)
}

/// What a page tells its reader about edits waiting on it: how many there
/// are, for a reviewer, and whether one is the reader's own.
pub(crate) async fn notice_for(
    db: &sqlx::PgPool,
    ctx: &Ctx,
    page_id: Uuid,
) -> Result<Option<minijinja::Value>, AppError> {
    let row = sqlx::query!(
        // bool_or over no rows is NULL: most pages have nothing waiting.
        r#"SELECT count(*) AS "n!",
                  COALESCE(bool_or(author_id IS NOT DISTINCT FROM $2), false) AS "mine!"
           FROM revisions WHERE page_id = $1 AND review_status = 'pending'"#,
        page_id,
        ctx.actor.user_id
    )
    .fetch_one(db)
    .await?;
    let reviewer = ctx.actor.can(Capability::RevisionPatrol);
    if row.n == 0 || !(reviewer || (row.mine && ctx.actor.user_id.is_some())) {
        return Ok(None);
    }
    Ok(Some(minijinja::context! {
        count => row.n,
        mine => row.mine && ctx.actor.user_id.is_some(),
        queue => reviewer.then(|| "/admin/review".to_string()),
    }))
}

/// A new page that exists only as edits waiting for review, for the page
/// view to explain instead of a 404: its title and whether the reader wrote it.
pub(crate) async fn pending_new_page(
    db: &sqlx::PgPool,
    ctx: &Ctx,
    namespace: &str,
    slug: &str,
) -> Result<Option<(String, bool)>, AppError> {
    let row = sqlx::query!(
        r#"SELECT r.review_title, (r.author_id IS NOT DISTINCT FROM $5) AS "mine!"
           FROM pages p JOIN revisions r ON r.page_id = p.id
           WHERE p.wiki_id = $1 AND p.namespace = ($2::text)::page_namespace AND p.slug = $3
             AND COALESCE(p.locale, '') = $4 AND p.current_revision_id IS NULL
             AND p.deleted_at IS NULL AND r.review_status = 'pending'
           ORDER BY r.created_at DESC LIMIT 1"#,
        ctx.wiki.id,
        namespace,
        slug,
        ctx.content_locale,
        ctx.actor.user_id
    )
    .fetch_optional(db)
    .await?;
    Ok(row.and_then(|r| {
        let mine = r.mine && ctx.actor.user_id.is_some();
        (mine || ctx.actor.can(Capability::RevisionPatrol))
            .then(|| (r.review_title.unwrap_or_default(), mine))
    }))
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct QueueQuery {
    #[serde(default)]
    tab: Option<String>,
    #[serde(default)]
    done: Option<String>,
}

/// GET /admin/review
pub async fn queue(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<QueueQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::RevisionPatrol).await
    );
    let tab = match query.tab.as_deref() {
        Some("patrol") => "patrol",
        _ => "pending",
    };
    let pending = pending_rows(&state, &ctx).await?;
    let patrol = if tab == "patrol" {
        patrol_rows(&state, &ctx).await?
    } else {
        Vec::new()
    };
    let done = query
        .done
        .as_deref()
        .filter(|d| ["accepted", "rejected", "rolled_back"].contains(d))
        .map(|d| ctx.t(&format!("review.done_{d}")));
    crate::admin::render(
        &ctx,
        "review",
        &ctx.t("review.title"),
        minijinja::context! {
            tab => tab,
            pending_count => pending.len(),
            pending => pending,
            patrol => patrol,
            patrol_days => PATROL_DAYS,
            done => done,
        },
    )
}

/// The page path and title of a queue row, and where it opens.
fn page_link(ctx: &Ctx, namespace: &str, slug: &str, locale: &str) -> (String, String) {
    let path = pages::path_of(namespace, slug).unwrap_or_else(|| slug.to_string());
    let href = if locale == ctx.content_locale || locale.is_empty() {
        ctx.link(&format!("/{path}"))
    } else {
        ctx.link_for(locale, &format!("/{path}"))
    };
    (path, href)
}

async fn pending_rows(state: &AppState, ctx: &Ctx) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT r.id, r.created_at, r.summary, r.review_title, r.base_revision_id,
                  octet_length(r.body_md) AS "bytes!",
                  (SELECT octet_length(b.body_md) FROM revisions b WHERE b.id = r.base_revision_id) AS "base_bytes?",
                  (SELECT u.username FROM users u WHERE u.id = r.author_id) AS "author?",
                  p.slug, p.namespace::text AS "namespace!", p.title, COALESCE(p.locale, '') AS "locale!",
                  p.current_revision_id
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE p.wiki_id = $1 AND r.review_status = 'pending' AND p.deleted_at IS NULL
           ORDER BY r.created_at
           LIMIT $2"#,
        ctx.wiki.id,
        QUEUE_SHOWN
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let (path, href) = page_link(ctx, &r.namespace, &r.slug, &r.locale);
            let is_new = r.base_revision_id.is_none();
            let outdated = !is_new && r.current_revision_id != r.base_revision_id;
            let delta = i64::from(r.bytes) - i64::from(r.base_bytes.unwrap_or(0));
            minijinja::context! {
                id => r.id.to_string(),
                title => r.review_title.clone().unwrap_or_else(|| r.title.clone()),
                path => path,
                href => (!is_new).then_some(href),
                author => r.author,
                summary => r.summary,
                at => ctx.day(r.created_at),
                time => r.created_at.format("%H:%M").to_string(),
                is_new => is_new,
                outdated => outdated,
                delta => delta,
                locale => (r.locale != ctx.content_locale).then_some(r.locale),
                open => format!("/admin/review/{}", r.id),
            }
        })
        .collect())
}

async fn patrol_rows(state: &AppState, ctx: &Ctx) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT r.id, r.created_at, r.summary,
                  (SELECT u.username FROM users u WHERE u.id = r.author_id) AS "author?",
                  (SELECT r2.id FROM revisions r2 WHERE r2.page_id = r.page_id
                     AND r2.review_status = 'accepted' AND (r2.created_at, r2.id) < (r.created_at, r.id)
                     ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1) AS "prev_id?",
                  p.slug, p.namespace::text AS "namespace!", p.title, COALESCE(p.locale, '') AS "locale!"
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE p.wiki_id = $1 AND r.review_status = 'accepted' AND NOT r.is_patrolled
             AND p.deleted_at IS NULL AND r.created_at > now() - make_interval(days => $3)
           ORDER BY r.created_at DESC
           LIMIT $2"#,
        ctx.wiki.id,
        QUEUE_SHOWN,
        PATROL_DAYS
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let (path, href) = page_link(ctx, &r.namespace, &r.slug, &r.locale);
            let base = href.trim_end_matches(&format!("/{path}")).to_string();
            minijinja::context! {
                id => r.id.to_string(),
                title => r.title,
                href => href.clone(),
                author => r.author,
                summary => r.summary,
                at => ctx.day(r.created_at),
                time => r.created_at.format("%H:%M").to_string(),
                diff => r.prev_id.map(|prev| format!("{href}/diff?from={prev}&to={}", r.id)),
                patrol_action => format!("{base}/{path}/patrol"),
            }
        })
        .collect())
}

/// One pending revision with its page, checked to belong to this wiki.
struct Pending {
    id: Uuid,
    page_id: Uuid,
    namespace: String,
    slug: String,
    locale: String,
    page_title: String,
    title: Option<String>,
    body_md: String,
    summary: Option<String>,
    author_id: Option<Uuid>,
    author: Option<String>,
    status: String,
    base: Option<Uuid>,
    base_body: Option<String>,
    current: Option<Uuid>,
    edit_level: Option<String>,
    locked: bool,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn load(db: &sqlx::PgPool, wiki_id: Uuid, id: Uuid) -> Result<Option<Pending>, AppError> {
    Ok(sqlx::query!(
        r#"SELECT r.id, r.page_id, r.body_md, r.summary, r.author_id, r.review_status,
                  r.review_title, r.base_revision_id, r.created_at,
                  (SELECT u.username FROM users u WHERE u.id = r.author_id) AS "author?",
                  (SELECT b.body_md FROM revisions b WHERE b.id = r.base_revision_id) AS "base_body?",
                  p.namespace::text AS "namespace!", p.slug, COALESCE(p.locale, '') AS "locale!",
                  p.title, p.current_revision_id, p.edit_level, p.is_locked
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE r.id = $1 AND p.wiki_id = $2 AND p.deleted_at IS NULL"#,
        id,
        wiki_id
    )
    .fetch_optional(db)
    .await?
    .map(|r| Pending {
        id: r.id,
        page_id: r.page_id,
        namespace: r.namespace,
        slug: r.slug,
        locale: r.locale,
        page_title: r.title,
        title: r.review_title,
        body_md: r.body_md,
        summary: r.summary,
        author_id: r.author_id,
        author: r.author,
        status: r.review_status,
        base: r.base_revision_id,
        base_body: r.base_body,
        current: r.current_revision_id,
        edit_level: r.edit_level,
        locked: r.is_locked,
        created_at: r.created_at,
    }))
}

impl Pending {
    fn path(&self) -> String {
        pages::path_of(&self.namespace, &self.slug).unwrap_or_else(|| self.slug.clone())
    }

    /// Edited since: accepting would drop what came in between.
    fn outdated(&self) -> bool {
        self.base.is_some() && self.current != self.base
    }

    /// The reviewer may edit the page at its protection level.
    fn reviewer_may_edit(&self, ctx: &Ctx) -> bool {
        let floor = matches!(self.namespace.as_str(), "template" | "page_template")
            .then_some(pages::TEMPLATE_EDIT_FLOOR);
        ctx.actor
            .can_edit_page(pages::protection_of(self.locked, self.edit_level.as_deref()).max(floor))
    }
}

/// GET /admin/review/{id}: the text, what it changes, and the two answers.
pub async fn show(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::RevisionPatrol).await
    );
    let Some(rev) = (match pages::parse_uuid(&id) {
        Some(id) => load(&state.db, ctx.wiki.id, id).await?,
        None => None,
    }) else {
        return Ok(crate::errors::not_found());
    };
    let path = rev.path();
    let expanded = crate::templates::expand(&state, &ctx, &path, &rev.body_md).await?;
    let rendered = naw_markdown::render_body(&expanded.text);
    let preview = crate::emotes::expand(&state, ctx.wiki.id, rendered.html).await?;
    let computed = crate::diff::diff_bodies(rev.base_body.as_deref().unwrap_or(""), &rev.body_md);
    let rows: Vec<minijinja::Value> = computed
        .rows
        .iter()
        .map(crate::history::row_value)
        .collect();
    let (_, href) = page_link(&ctx, &rev.namespace, &rev.slug, &rev.locale);
    crate::admin::render(
        &ctx,
        "review_item",
        &ctx.t("review.item_title"),
        minijinja::context! {
            id => rev.id.to_string(),
            title => rev.title.clone().unwrap_or_else(|| rev.page_title.clone()),
            renamed => rev.title.as_ref().filter(|t| **t != rev.page_title).map(|_| rev.page_title.clone()),
            path => path,
            href => rev.current.is_some().then_some(href),
            author => rev.author.clone(),
            summary => rev.summary.clone(),
            at => ctx.day(rev.created_at),
            status => rev.status.clone(),
            status_label => ctx.t(&format!("review.status_{}", rev.status)),
            is_new => rev.base.is_none(),
            outdated => rev.outdated(),
            may_decide => rev.status == "pending" && rev.reviewer_may_edit(&ctx),
            preview => preview,
            rows => rows,
            added => computed.added,
            removed => computed.removed,
            truncated => computed.truncated,
        },
    )
}

/// POST /admin/review/{id}/accept
pub async fn accept(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::RevisionPatrol).await
    );
    let Some(rev) = (match pages::parse_uuid(&id) {
        Some(id) => load(&state.db, ctx.wiki.id, id).await?,
        None => None,
    }) else {
        return Ok(crate::errors::not_found());
    };
    let back = format!("/admin/review/{}", rev.id);
    if rev.status != "pending" {
        return Ok(pages::see_other(&back));
    }
    if !rev.reviewer_may_edit(&ctx) {
        return refuse(&ctx, &back, StatusCode::FORBIDDEN, "review.not_yours");
    }
    if rev.outdated() {
        return refuse(&ctx, &back, StatusCode::CONFLICT, "review.outdated");
    }
    let path = rev.path();
    let title = rev.title.clone().unwrap_or_else(|| rev.page_title.clone());
    let prepared = pages::prepare(&state, &ctx, &path, &rev.body_md).await?;
    let mut tx = state.db.begin().await?;
    let marked = sqlx::query!(
        "UPDATE revisions SET review_status = 'accepted', reviewed_by = $2, reviewed_at = now(),
                              is_patrolled = true
         WHERE id = $1 AND review_status = 'pending'",
        rev.id,
        ctx.actor.user_id
    )
    .execute(&mut *tx)
    .await?;
    // Compare and swap: the page must still stand where the edit began.
    let swapped = sqlx::query!(
        "UPDATE pages SET current_revision_id = $1, title = $2, updated_at = now()
         WHERE id = $3 AND current_revision_id IS NOT DISTINCT FROM $4",
        rev.id,
        title,
        rev.page_id,
        rev.current
    )
    .execute(&mut *tx)
    .await?;
    if marked.rows_affected() == 0 || swapped.rows_affected() == 0 {
        return refuse(&ctx, &back, StatusCode::CONFLICT, "review.outdated");
    }
    pages::index(
        &mut tx,
        &ctx,
        rev.page_id,
        &rev.locale,
        &title,
        rev.summary.as_deref(),
        &prepared,
    )
    .await?;
    tx.commit().await?;
    pages::after_save(&state, &ctx, rev.page_id, &prepared).await;
    if rev.namespace == "template" {
        crate::indexing::refresh_users_of(&state, &ctx, &rev.slug);
    }
    audit(
        &state,
        &ctx,
        "revision.accept",
        &rev,
        json!({ "path": path, "new_page": rev.base.is_none() }),
    )
    .await;
    // Watchers hear of the edit now that readers see it; its author hears it
    // went through.
    crate::notify::page_edited(
        &state.db,
        ctx.wiki.id,
        &path,
        rev.author_id,
        ctx.actor.user_id,
        &title,
        &crate::notify::history_link(&ctx, &rev.locale, &path),
        rev.summary.as_deref(),
    )
    .await;
    if let Some(author) = rev.author_id {
        let (_, href) = page_link(&ctx, &rev.namespace, &rev.slug, &rev.locale);
        crate::notify::push(
            &state.db,
            author,
            ctx.wiki.id,
            &crate::notify::Note {
                kind: "edit_accepted",
                actor_id: ctx.actor.user_id,
                title: &title,
                link: &href,
                note: None,
            },
        )
        .await;
    }
    Ok(pages::see_other("/admin/review?done=accepted"))
}

#[derive(Debug, serde::Deserialize)]
pub struct RejectForm {
    #[serde(default)]
    note: String,
}

/// POST /admin/review/{id}/reject: the edit stays in the history as
/// rejected, with the reviewer's note for its author. A new page that had
/// nothing but rejected edits is removed, so its address is free again.
pub async fn reject(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<RejectForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::RevisionPatrol).await
    );
    let Some(rev) = (match pages::parse_uuid(&id) {
        Some(id) => load(&state.db, ctx.wiki.id, id).await?,
        None => None,
    }) else {
        return Ok(crate::errors::not_found());
    };
    let back = format!("/admin/review/{}", rev.id);
    if rev.status != "pending" {
        return Ok(pages::see_other(&back));
    }
    if !rev.reviewer_may_edit(&ctx) {
        return refuse(&ctx, &back, StatusCode::FORBIDDEN, "review.not_yours");
    }
    let note: String = form
        .note
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(ctx.limits.file_note_chars.max(500))
        .collect();
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "UPDATE revisions SET review_status = 'rejected', reviewed_by = $2, reviewed_at = now(),
                              review_note = $3
         WHERE id = $1 AND review_status = 'pending'",
        rev.id,
        ctx.actor.user_id,
        (!note.is_empty()).then_some(note.as_str())
    )
    .execute(&mut *tx)
    .await?;
    let removed = sqlx::query!(
        "DELETE FROM pages p WHERE p.id = $1 AND p.current_revision_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM revisions r WHERE r.page_id = p.id AND r.review_status = 'pending')",
        rev.page_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    tx.commit().await?;
    audit(
        &state,
        &ctx,
        "revision.reject",
        &rev,
        json!({ "path": rev.path(), "note": note, "removed_new_page": removed }),
    )
    .await;
    if let Some(author) = rev.author_id {
        // The history keeps the rejected edit and the note; a removed new
        // page has no history left to open.
        let link = if removed {
            String::new()
        } else {
            crate::notify::history_link(&ctx, &rev.locale, &rev.path())
        };
        let title = rev.title.clone().unwrap_or_else(|| rev.page_title.clone());
        crate::notify::push(
            &state.db,
            author,
            ctx.wiki.id,
            &crate::notify::Note {
                kind: "edit_rejected",
                actor_id: ctx.actor.user_id,
                title: &title,
                link: &link,
                note: (!note.is_empty()).then_some(note.as_str()),
            },
        )
        .await;
    }
    Ok(pages::see_other("/admin/review?done=rejected"))
}

fn refuse(ctx: &Ctx, back: &str, status: StatusCode, key: &str) -> Result<Response, AppError> {
    pages::notice(
        ctx,
        status,
        &ctx.t("review.refused"),
        &ctx.t(key),
        back,
        &ctx.t("review.back"),
    )
}

async fn audit(
    state: &AppState,
    ctx: &Ctx,
    action: &'static str,
    rev: &Pending,
    meta: serde_json::Value,
) {
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action,
            entity_type: "revision",
            entity_id: Some(rev.id),
            meta: json!({ "page": rev.page_id, "author": rev.author_id, "detail": meta }),
        },
    )
    .await;
}

/// POST /{slug}/rollback: every edit the last author made in a row, undone
/// at once, back to the version before them. Curators and up.
pub async fn rollback(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    if !pages::slug_is_valid(&slug) {
        return Ok(crate::errors::not_found());
    }
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let history = ctx.link(&format!("/{slug}/history"));
    if !ctx.actor.can(Capability::RevisionPatrol) {
        return refuse(
            &ctx,
            &history,
            StatusCode::FORBIDDEN,
            "review.rollback_refused",
        );
    }
    let Some(found) = pages::find_page(&state.db, ctx.wiki.id, &slug, &ctx.content_locale).await?
    else {
        return Ok(crate::errors::not_found());
    };
    if !ctx.actor.can_edit_page(found.protection) {
        return refuse(
            &ctx,
            &history,
            StatusCode::FORBIDDEN,
            "review.rollback_refused",
        );
    }
    let Some(last) = sqlx::query!(
        r#"SELECT r.author_id, (SELECT u.username FROM users u WHERE u.id = r.author_id) AS "author?"
           FROM revisions r WHERE r.id = $1"#,
        found.revision_id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    // The newest accepted version by anyone else.
    let Some(target) = sqlx::query!(
        "SELECT id, body_md FROM revisions
         WHERE page_id = $1 AND review_status = 'accepted' AND author_id IS DISTINCT FROM $2
         ORDER BY created_at DESC, id DESC LIMIT 1",
        found.id,
        last.author_id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return refuse(
            &ctx,
            &history,
            StatusCode::CONFLICT,
            "review.rollback_nothing",
        );
    };
    let who = last.author.unwrap_or_else(|| ctx.t("history.anonymous"));
    let summary = ctx.t_with("review.rollback_summary", &[("who", &who)]);
    if let Some(conflict) = crate::history::restore(
        &state,
        &ctx,
        &slug,
        &found,
        target.id,
        &target.body_md,
        &summary,
        "page.rollback",
    )
    .await?
    {
        return Ok(conflict);
    }
    Ok(pages::see_other(&format!("{history}?done=rolled_back")))
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::PgPool;

    async fn page_with_text(db: &PgPool, body: &str) -> (Uuid, Uuid, Uuid) {
        let wiki = Uuid::new_v4();
        sqlx::query("INSERT INTO wikis (id, slug, name) VALUES ($1, 'w', 'w')")
            .bind(wiki)
            .execute(db)
            .await
            .expect("wiki");
        let page = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale) VALUES ($1, $2, 'main', 'a', 'A', 'en')",
        )
        .bind(page)
        .bind(wiki)
        .execute(db)
        .await
        .expect("page");
        let rev = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO revisions (id, page_id, body_md, content_hash) VALUES ($1, $2, $3, '\\x00')",
        )
        .bind(rev)
        .bind(page)
        .bind(body)
        .execute(db)
        .await
        .expect("revision");
        sqlx::query("UPDATE pages SET current_revision_id = $2 WHERE id = $1")
            .bind(page)
            .bind(rev)
            .execute(db)
            .await
            .expect("current");
        (wiki, page, rev)
    }

    /// Every page view asks; nearly every page has nothing waiting.
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_page_with_nothing_waiting_reads_as_nothing(db: PgPool) {
        let (_, page, _) = page_with_text(&db, "text").await;
        let row = sqlx::query!(
            r#"SELECT count(*) AS "n!",
                      COALESCE(bool_or(author_id IS NOT DISTINCT FROM $2), false) AS "mine!"
               FROM revisions WHERE page_id = $1 AND review_status = 'pending'"#,
            page,
            None::<Uuid>
        )
        .fetch_one(&db)
        .await
        .expect("no pending rows still decode");
        assert_eq!((row.n, row.mine), (0, false));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_pending_edit_leaves_the_page_as_it_was(db: PgPool) {
        let (wiki, page, rev) = page_with_text(&db, "accepted text").await;
        let mut conn = db.acquire().await.expect("conn");
        let pending = save_pending(
            &mut conn,
            page,
            Some(rev),
            None,
            "New title",
            None,
            "new text",
            false,
        )
        .await
        .expect("pending");
        let current: Option<Uuid> =
            sqlx::query_scalar("SELECT current_revision_id FROM pages WHERE id = $1")
                .bind(page)
                .fetch_one(&db)
                .await
                .expect("current");
        assert_eq!(current, Some(rev), "readers keep the accepted text");
        let title: String = sqlx::query_scalar("SELECT title FROM pages WHERE id = $1")
            .bind(page)
            .fetch_one(&db)
            .await
            .expect("title");
        assert_eq!(title, "A", "and the accepted title");
        let loaded = load(&db, wiki, pending)
            .await
            .expect("load")
            .expect("found");
        assert_eq!(loaded.status, "pending");
        assert_eq!(loaded.title.as_deref(), Some("New title"));
        assert!(!loaded.outdated(), "written over the current text");
        assert!(
            load(&db, Uuid::new_v4(), pending)
                .await
                .expect("load")
                .is_none(),
            "another wiki cannot open it"
        );
        // Somebody with the pass edits in between: the pending edit is now stale.
        let other = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO revisions (id, page_id, body_md, content_hash) VALUES ($1, $2, 'later', '\\x00')",
        )
        .bind(other)
        .bind(page)
        .execute(&db)
        .await
        .expect("later");
        sqlx::query("UPDATE pages SET current_revision_id = $2 WHERE id = $1")
            .bind(page)
            .bind(other)
            .execute(&db)
            .await
            .expect("moved on");
        let stale = load(&db, wiki, pending)
            .await
            .expect("load")
            .expect("found");
        assert!(
            stale.outdated(),
            "accepting it now would drop the later edit"
        );
    }
}
