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

/// The reader's own new page at `path` that waits for review, with its
/// newest waiting version, so its author can keep working on it.
pub(crate) struct OwnPending {
    pub page_id: Uuid,
    pub title: String,
    pub summary: Option<String>,
    pub body_md: String,
}

pub(crate) async fn own_pending_page(
    db: &sqlx::PgPool,
    ctx: &Ctx,
    path: &str,
) -> Result<Option<OwnPending>, AppError> {
    let Some(me) = ctx.actor.user_id else {
        return Ok(None);
    };
    let (namespace, slug) = pages::split_path(path);
    let row = sqlx::query!(
        r#"SELECT p.id, p.title, r.review_title, r.summary, r.body_md
           FROM pages p JOIN revisions r ON r.page_id = p.id
           WHERE p.wiki_id = $1 AND p.namespace = ($2::text)::page_namespace AND p.slug = $3
             AND COALESCE(p.locale, '') = $4 AND p.current_revision_id IS NULL
             AND p.deleted_at IS NULL AND r.review_status = 'pending' AND r.author_id = $5
           ORDER BY r.created_at DESC LIMIT 1"#,
        ctx.wiki.id,
        namespace,
        slug,
        ctx.content_locale,
        me
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| OwnPending {
        page_id: r.id,
        title: r.review_title.unwrap_or(r.title),
        summary: r.summary,
        body_md: r.body_md,
    }))
}

/// Replaces the author's waiting versions of their new page with a fresh
/// one, inside the caller's transaction: the old ones stay in the history as
/// set aside by their author, and the queue holds only the newest.
pub(crate) async fn replace_own_pending(
    conn: &mut sqlx::PgConnection,
    page_id: Uuid,
    author: Uuid,
    note: &str,
    title: &str,
    summary: Option<&str>,
    body_md: &str,
) -> Result<Uuid, AppError> {
    sqlx::query!(
        "UPDATE revisions SET review_status = 'rejected', reviewed_by = $2, reviewed_at = now(),
                              review_note = $3
         WHERE page_id = $1 AND author_id = $2 AND review_status = 'pending'",
        page_id,
        author,
        note
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!("UPDATE pages SET title = $2 WHERE id = $1", page_id, title)
        .execute(&mut *conn)
        .await?;
    save_pending(
        conn,
        page_id,
        None,
        Some(author),
        title,
        summary,
        body_md,
        false,
    )
    .await
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct QueueQuery {
    #[serde(default)]
    tab: Option<String>,
    #[serde(default)]
    done: Option<String>,
    /// Which waiting edits: all, new pages, edits, or the reviewer's wards.
    #[serde(default)]
    show: Option<String>,
    /// After a bulk decision: how many went through and how many were skipped.
    #[serde(default)]
    n: Option<u32>,
    #[serde(default)]
    skipped: Option<u32>,
}

/// The queue's filters, as `show` spells them.
const SHOWS: [&str; 4] = ["all", "new", "edits", "wards"];

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
    let show = query
        .show
        .as_deref()
        .filter(|s| SHOWS.contains(s))
        .unwrap_or("all");
    let pending = pending_rows(&state, &ctx, show).await?;
    let has_wards = match ctx.actor.user_id {
        Some(me) => sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM curatorships WHERE wiki_id = $1 AND curator_id = $2) AS "x!""#,
            ctx.wiki.id,
            me
        )
        .fetch_one(&state.db)
        .await?,
        None => false,
    };
    let patrol = if tab == "patrol" {
        patrol_rows(&state, &ctx).await?
    } else {
        Vec::new()
    };
    let done = query
        .done
        .as_deref()
        .filter(|d| {
            [
                "accepted",
                "rejected",
                "rolled_back",
                "bulk_accepted",
                "bulk_rejected",
            ]
            .contains(d)
        })
        .map(|d| {
            let n = query.n.unwrap_or(0).to_string();
            let skipped = query.skipped.unwrap_or(0);
            let mut text = ctx.t_with(&format!("review.done_{d}"), &[("n", &n)]);
            if skipped > 0 {
                text.push(' ');
                text.push_str(&ctx.t_with("review.done_skipped", &[("n", &skipped.to_string())]));
            }
            text
        });
    crate::admin::render(
        &ctx,
        "review",
        &ctx.t("review.title"),
        minijinja::context! {
            tab => tab,
            show => show,
            shows => SHOWS,
            has_wards => has_wards,
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

/// Whether this reviewer checks edits by `author` (`None` for a guest), as
/// the wiki's review scope says: see [`crate::perm::ReviewScope`].
pub(crate) async fn checks(
    db: &sqlx::PgPool,
    ctx: &Ctx,
    author: Option<Uuid>,
) -> Result<bool, AppError> {
    checks_on(db, ctx.wiki.id, ctx.actor.review_bound(), author).await
}

/// [`checks`] for a reviewer bound as [`crate::perm::Actor::review_bound`] says.
async fn checks_on(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    bound: Option<(Uuid, bool)>,
    author: Option<Uuid>,
) -> Result<bool, AppError> {
    let Some((me, unassigned)) = bound else {
        return Ok(true);
    };
    let curator = match author {
        Some(author) => {
            sqlx::query_scalar!(
                "SELECT curator_id FROM curatorships WHERE wiki_id = $1 AND user_id = $2",
                wiki_id,
                author
            )
            .fetch_optional(db)
            .await?
        }
        None => None,
    };
    Ok(curator.map_or(unassigned, |c| c == me))
}

/// The queue filter's parameters: the curator whose wards a bound reviewer
/// sees (`None` for everyone's) and whether people nobody looks after count.
fn bound_params(ctx: &Ctx) -> (Option<Uuid>, bool) {
    ctx.actor
        .review_bound()
        .map_or((None, true), |(me, unassigned)| (Some(me), unassigned))
}

/// How many edits wait for a reviewer bound as
/// [`crate::perm::Actor::review_bound`] says, for the menu's count.
pub(crate) async fn queue_count(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    bound: Option<(Uuid, bool)>,
) -> Result<i64, AppError> {
    let (bound, unassigned) = bound.map_or((None, true), |(me, u)| (Some(me), u));
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!"
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE p.wiki_id = $1 AND r.review_status = 'pending' AND p.deleted_at IS NULL
             AND ($2::uuid IS NULL
                  OR EXISTS (SELECT 1 FROM curatorships c WHERE c.wiki_id = p.wiki_id
                               AND c.user_id = r.author_id AND c.curator_id = $2)
                  OR ($3 AND NOT EXISTS (SELECT 1 FROM curatorships c WHERE c.wiki_id = p.wiki_id
                                           AND c.user_id = r.author_id)))"#,
        wiki_id,
        bound,
        unassigned
    )
    .fetch_one(db)
    .await?)
}

async fn pending_rows(
    state: &AppState,
    ctx: &Ctx,
    show: &str,
) -> Result<Vec<minijinja::Value>, AppError> {
    let (bound, unassigned) = bound_params(ctx);
    let rows = sqlx::query!(
        r#"SELECT r.id, r.created_at, r.summary, r.review_title, r.base_revision_id,
                  octet_length(r.body_md) AS "bytes!",
                  (SELECT octet_length(b.body_md) FROM revisions b WHERE b.id = r.base_revision_id) AS "base_bytes?",
                  (SELECT u.username FROM users u WHERE u.id = r.author_id) AS "author?",
                  (SELECT cu.username FROM curatorships c JOIN users cu ON cu.id = c.curator_id
                     WHERE c.wiki_id = p.wiki_id AND c.user_id = r.author_id) AS "curator?",
                  p.slug, p.namespace::text AS "namespace!", p.title, COALESCE(p.locale, '') AS "locale!",
                  p.current_revision_id
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE p.wiki_id = $1 AND r.review_status = 'pending' AND p.deleted_at IS NULL
             AND ($3::uuid IS NULL
                  OR EXISTS (SELECT 1 FROM curatorships c WHERE c.wiki_id = p.wiki_id
                               AND c.user_id = r.author_id AND c.curator_id = $3)
                  OR ($4 AND NOT EXISTS (SELECT 1 FROM curatorships c WHERE c.wiki_id = p.wiki_id
                                           AND c.user_id = r.author_id)))
             AND CASE $5::text
                   WHEN 'new' THEN r.base_revision_id IS NULL
                   WHEN 'edits' THEN r.base_revision_id IS NOT NULL
                   WHEN 'wards' THEN EXISTS (SELECT 1 FROM curatorships c WHERE c.wiki_id = p.wiki_id
                                               AND c.user_id = r.author_id AND c.curator_id = $6)
                   ELSE true
                 END
           ORDER BY r.created_at
           LIMIT $2"#,
        ctx.wiki.id,
        QUEUE_SHOWN,
        bound,
        unassigned,
        show,
        ctx.actor.user_id
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
                curator => r.curator,
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
    let (bound, unassigned) = bound_params(ctx);
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
             AND ($4::uuid IS NULL
                  OR EXISTS (SELECT 1 FROM curatorships c WHERE c.wiki_id = p.wiki_id
                               AND c.user_id = r.author_id AND c.curator_id = $4)
                  OR ($5 AND NOT EXISTS (SELECT 1 FROM curatorships c WHERE c.wiki_id = p.wiki_id
                                           AND c.user_id = r.author_id)))
           ORDER BY r.created_at DESC
           LIMIT $2"#,
        ctx.wiki.id,
        QUEUE_SHOWN,
        PATROL_DAYS,
        bound,
        unassigned
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
        let floor = pages::namespace_floor(&self.namespace);
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
    let preview = crate::diagrams::expand(&state, &ctx, preview, false).await?;
    let preview = crate::thumbs::expand(&state, preview).await?;
    let computed = crate::diff::diff_bodies(rev.base_body.as_deref().unwrap_or(""), &rev.body_md);
    let rows: Vec<minijinja::Value> = computed
        .rows
        .iter()
        .map(crate::history::row_value)
        .collect();
    let (_, href) = page_link(&ctx, &rev.namespace, &rev.slug, &rev.locale);
    let ward = checks(&state.db, &ctx, rev.author_id).await?;
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
            may_decide => rev.status == "pending" && rev.reviewer_may_edit(&ctx) && ward,
            not_ward => rev.status == "pending" && !ward,
            preview => preview,
            rows => rows,
            added => computed.added,
            removed => computed.removed,
            truncated => computed.truncated,
        },
    )
}

/// What deciding one waiting edit came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decided {
    Done,
    /// Somebody decided it already, or its author took it back.
    NotPending,
    /// The page is protected above this reviewer.
    NotYours,
    /// Not among the edits this reviewer checks.
    NotWard,
    /// The page moved on since the edit was written.
    Outdated,
}

impl Decided {
    /// The refusal for one edit: its status code and message key.
    fn refusal(self) -> Option<(StatusCode, &'static str)> {
        match self {
            Self::Done | Self::NotPending => None,
            Self::NotYours => Some((StatusCode::FORBIDDEN, "review.not_yours")),
            Self::NotWard => Some((StatusCode::FORBIDDEN, "review.not_ward")),
            Self::Outdated => Some((StatusCode::CONFLICT, "review.outdated")),
        }
    }
}

/// A reviewer's note, cleaned and bounded.
fn clean_note(ctx: &Ctx, raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(ctx.limits.file_note_chars.max(500))
        .collect()
}

/// Whether this reviewer may decide `rev` at all.
async fn may_decide(state: &AppState, ctx: &Ctx, rev: &Pending) -> Result<Decided, AppError> {
    if rev.status != "pending" {
        return Ok(Decided::NotPending);
    }
    if !rev.reviewer_may_edit(ctx) {
        return Ok(Decided::NotYours);
    }
    if !checks(&state.db, ctx, rev.author_id).await? {
        return Ok(Decided::NotWard);
    }
    Ok(Decided::Done)
}

/// Accepts `rev`: it becomes the page as a save would, its watchers and its
/// author hear of it, and an author with enough accepted edits gets the pass.
async fn accept_one(
    state: &AppState,
    ctx: &Ctx,
    rev: &Pending,
    note: &str,
) -> Result<Decided, AppError> {
    let allowed = may_decide(state, ctx, rev).await?;
    if allowed != Decided::Done {
        return Ok(allowed);
    }
    if rev.outdated() {
        return Ok(Decided::Outdated);
    }
    let path = rev.path();
    let title = rev.title.clone().unwrap_or_else(|| rev.page_title.clone());
    let prepared = pages::prepare(state, ctx, &path, &rev.body_md).await?;
    let mut tx = state.db.begin().await?;
    let marked = sqlx::query!(
        "UPDATE revisions SET review_status = 'accepted', reviewed_by = $2, reviewed_at = now(),
                              is_patrolled = true, review_note = $3
         WHERE id = $1 AND review_status = 'pending'",
        rev.id,
        ctx.actor.user_id,
        (!note.is_empty()).then_some(note)
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
    if marked.rows_affected() == 0 {
        return Ok(Decided::NotPending);
    }
    if swapped.rows_affected() == 0 {
        return Ok(Decided::Outdated);
    }
    pages::index(
        &mut tx,
        ctx,
        rev.page_id,
        &rev.locale,
        &title,
        rev.summary.as_deref(),
        &prepared,
    )
    .await?;
    tx.commit().await?;
    pages::after_save(state, ctx, rev.page_id, &prepared).await;
    if rev.namespace == "template" {
        crate::indexing::refresh_users_of(state, ctx, &rev.slug);
    }
    audit(
        state,
        ctx,
        "revision.accept",
        rev,
        json!({ "path": path, "new_page": rev.base.is_none(), "note": note }),
    )
    .await;
    // Watchers hear of the edit now that readers see it; its author hears it
    // went through, with the reviewer's word if there was one.
    crate::notify::page_edited(
        &state.db,
        ctx.wiki.id,
        &path,
        rev.author_id,
        ctx.actor.user_id,
        &title,
        &crate::notify::history_link(ctx, &rev.locale, &path),
        rev.summary.as_deref(),
    )
    .await;
    if let Some(author) = rev.author_id {
        let (_, href) = page_link(ctx, &rev.namespace, &rev.slug, &rev.locale);
        crate::notify::push(
            &state.db,
            author,
            ctx.wiki.id,
            &crate::notify::Note {
                kind: "edit_accepted",
                actor_id: ctx.actor.user_id,
                title: &title,
                link: &href,
                note: (!note.is_empty()).then_some(note),
            },
        )
        .await;
        trust_if_earned(state, ctx, author).await?;
    }
    crate::notify::waiting_settled(&state.db, ctx.wiki.id).await;
    Ok(Decided::Done)
}

/// Gives `author` the pass once the wiki's `review.trust_after` edits of
/// theirs were accepted, unless somebody set the pass for them by hand.
async fn trust_if_earned(state: &AppState, ctx: &Ctx, author: Uuid) -> Result<(), AppError> {
    let needed = ctx.actor.rules.review_trust_after;
    if needed == 0 {
        return Ok(());
    }
    let granted = sqlx::query_scalar!(
        r#"INSERT INTO user_capabilities (user_id, wiki_id, capability, allowed, set_by)
           SELECT $1, $2, 'edit.unreviewed', true, NULL
           WHERE (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id
                  WHERE r.author_id = $1 AND p.wiki_id = $2
                    AND r.review_status = 'accepted' AND r.reviewed_by IS NOT NULL) >= $3
           ON CONFLICT (user_id, wiki_id, capability) DO NOTHING
           RETURNING user_id"#,
        author,
        ctx.wiki.id,
        i64::from(needed)
    )
    .fetch_optional(&state.db)
    .await?;
    if granted.is_some() {
        crate::audit::record_or_log(
            &state.db,
            crate::audit::Entry {
                wiki_id: Some(ctx.wiki.id),
                user_id: None,
                action: "user.trusted",
                entity_type: "user",
                entity_id: Some(author),
                meta: json!({ "accepted": needed }),
            },
        )
        .await;
        crate::notify::push(
            &state.db,
            author,
            ctx.wiki.id,
            &crate::notify::Note {
                kind: "edit_trusted",
                actor_id: None,
                title: &ctx.wiki.name,
                link: "",
                note: None,
            },
        )
        .await;
    }
    Ok(())
}

/// Rejects `rev` with `note` for its author. A new page that had nothing
/// but rejected edits is removed, so its address is free again.
async fn reject_one(
    state: &AppState,
    ctx: &Ctx,
    rev: &Pending,
    note: &str,
) -> Result<Decided, AppError> {
    let allowed = may_decide(state, ctx, rev).await?;
    if allowed != Decided::Done {
        return Ok(allowed);
    }
    let mut tx = state.db.begin().await?;
    let marked = sqlx::query!(
        "UPDATE revisions SET review_status = 'rejected', reviewed_by = $2, reviewed_at = now(),
                              review_note = $3
         WHERE id = $1 AND review_status = 'pending'",
        rev.id,
        ctx.actor.user_id,
        (!note.is_empty()).then_some(note)
    )
    .execute(&mut *tx)
    .await?;
    if marked.rows_affected() == 0 {
        return Ok(Decided::NotPending);
    }
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
        state,
        ctx,
        "revision.reject",
        rev,
        json!({ "path": rev.path(), "note": note, "removed_new_page": removed }),
    )
    .await;
    if let Some(author) = rev.author_id {
        // The history keeps the rejected edit and the note; a removed new
        // page has no history left to open.
        let link = if removed {
            String::new()
        } else {
            crate::notify::history_link(ctx, &rev.locale, &rev.path())
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
                note: (!note.is_empty()).then_some(note),
            },
        )
        .await;
    }
    crate::notify::waiting_settled(&state.db, ctx.wiki.id).await;
    Ok(Decided::Done)
}

#[derive(Debug, serde::Deserialize)]
pub struct DecideForm {
    #[serde(default)]
    note: String,
}

/// One edit's page after a decision: back to the queue, or why not.
fn after_one(ctx: &Ctx, rev: &Pending, outcome: Decided, done: &str) -> Result<Response, AppError> {
    let back = format!("/admin/review/{}", rev.id);
    match outcome.refusal() {
        Some((status, key)) => refuse(ctx, &back, status, key),
        None if outcome == Decided::NotPending => Ok(pages::see_other(&back)),
        None => Ok(pages::see_other(&format!("/admin/review?done={done}"))),
    }
}

/// POST /admin/review/{id}/accept, with an optional note for the author.
pub async fn accept(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<DecideForm>,
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
    let outcome = accept_one(&state, &ctx, &rev, &clean_note(&ctx, &form.note)).await?;
    after_one(&ctx, &rev, outcome, "accepted")
}

/// POST /admin/review/{id}/reject: the edit stays in the history as
/// rejected, with the reviewer's note for its author.
pub async fn reject(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<DecideForm>,
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
    let outcome = reject_one(&state, &ctx, &rev, &clean_note(&ctx, &form.note)).await?;
    after_one(&ctx, &rev, outcome, "rejected")
}

/// Edits one bulk decision takes at most.
const BULK_MAX: usize = 100;

/// POST /admin/review/bulk: `action` (accept or reject), an optional
/// `note`, and one `id` per chosen edit. Each edit is decided on its own,
/// so one that moved on or is somebody else's is skipped, not fatal.
pub async fn bulk(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::RevisionPatrol).await
    );
    let mut ids = Vec::new();
    let mut action = String::new();
    let mut note = String::new();
    for (key, value) in url::form_urlencoded::parse(&body) {
        match key.as_ref() {
            "id" if ids.len() < BULK_MAX => ids.extend(pages::parse_uuid(&value)),
            "action" => action = value.into_owned(),
            "note" => note = value.into_owned(),
            _ => {}
        }
    }
    let accepting = match action.as_str() {
        "accept" => true,
        "reject" => false,
        _ => return Ok(pages::see_other("/admin/review")),
    };
    let note = clean_note(&ctx, &note);
    let (mut done, mut skipped) = (0, 0);
    for id in ids {
        let Some(rev) = load(&state.db, ctx.wiki.id, id).await? else {
            skipped += 1;
            continue;
        };
        let outcome = if accepting {
            accept_one(&state, &ctx, &rev, &note).await?
        } else {
            reject_one(&state, &ctx, &rev, &note).await?
        };
        if outcome == Decided::Done {
            done += 1;
        } else {
            skipped += 1;
        }
    }
    let verb = if accepting { "accepted" } else { "rejected" };
    Ok(pages::see_other(&format!(
        "/admin/review?done=bulk_{verb}&n={done}&skipped={skipped}"
    )))
}

/// Days a turned-down edit stays on its author's list.
const REJECTED_SHOWN_DAYS: i32 = 30;

/// How many of their own edits an author's list shows.
const MINE_SHOWN: i64 = 50;

/// The author's edits here that wait for review, and those turned down in
/// the last [`REJECTED_SHOWN_DAYS`], newest first.
pub(crate) async fn mine(
    state: &AppState,
    ctx: &Ctx,
    user_id: Uuid,
) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT r.id, r.review_status, r.review_note, r.summary, r.created_at, r.review_title,
                  r.base_revision_id, p.namespace::text AS "namespace!", p.slug, p.title,
                  COALESCE(p.locale, '') AS "locale!"
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE r.author_id = $1 AND r.review_status <> 'accepted'
             AND p.wiki_id = $2 AND p.deleted_at IS NULL
             AND (r.review_status = 'pending'
                  OR r.reviewed_at > now() - make_interval(days => $3))
           ORDER BY r.created_at DESC
           LIMIT $4"#,
        user_id,
        ctx.wiki.id,
        REJECTED_SHOWN_DAYS,
        MINE_SHOWN
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let (path, href) = page_link(ctx, &r.namespace, &r.slug, &r.locale);
            let pending = r.review_status == "pending";
            minijinja::context! {
                title => r.review_title.unwrap_or(r.title),
                path => path.clone(),
                href => href.clone(),
                history_href => crate::notify::history_link(ctx, &r.locale, &path),
                is_new => r.base_revision_id.is_none(),
                status => r.review_status.clone(),
                status_label => ctx.t(&format!("review.status_{}", r.review_status)),
                note => r.review_note.filter(|n| !n.trim().is_empty()),
                summary => r.summary.filter(|s| !s.trim().is_empty()),
                at => format!("{}, {} UTC", ctx.day(r.created_at), r.created_at.format("%H:%M")),
                edit_href => (pending && r.base_revision_id.is_none()).then(|| format!("{href}/edit")),
                withdraw_action => pending.then(|| format!("/drafts/review/{}/withdraw", r.id)),
            }
        })
        .collect())
}

/// Turns the author's waiting edit `id` down in their name, inside the
/// caller's transaction, and removes a new page left with nothing. The page
/// and whether it went; `None` unless it is this author's, in this wiki, and
/// still waiting.
async fn take_back(
    conn: &mut sqlx::PgConnection,
    wiki_id: Uuid,
    author: Uuid,
    id: Uuid,
    note: &str,
) -> Result<Option<(Uuid, bool)>, AppError> {
    let Some(page_id) = sqlx::query_scalar!(
        "UPDATE revisions r SET review_status = 'rejected', reviewed_by = $2, reviewed_at = now(),
                                review_note = $3
         FROM pages p
         WHERE r.id = $1 AND r.page_id = p.id AND p.wiki_id = $4
           AND r.author_id = $2 AND r.review_status = 'pending'
         RETURNING r.page_id",
        id,
        author,
        note,
        wiki_id
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let removed = sqlx::query!(
        "DELETE FROM pages p WHERE p.id = $1 AND p.current_revision_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM revisions r WHERE r.page_id = p.id AND r.review_status = 'pending')",
        page_id
    )
    .execute(&mut *conn)
    .await?
    .rows_affected()
        > 0;
    Ok(Some((page_id, removed)))
}

/// POST /drafts/review/{id}/withdraw: the author takes back an edit that
/// still waits. It stays in the history as turned down, by its author; a new
/// page with nothing else waiting goes, so its address is free again.
pub async fn withdraw(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(pages::see_other("/login?next=%2Fdrafts"));
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, Some(&user)).await?);
    let Some(id) = pages::parse_uuid(&id) else {
        return Ok(crate::errors::not_found());
    };
    let note = ctx.t("review.withdrawn_note");
    let mut tx = state.db.begin().await?;
    let Some((page_id, removed)) = take_back(&mut tx, ctx.wiki.id, user.id, id, &note).await?
    else {
        return Ok(crate::errors::not_found());
    };
    tx.commit().await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: Some(user.id),
            action: "revision.withdraw",
            entity_type: "revision",
            entity_id: Some(id),
            meta: json!({ "page": page_id, "removed_new_page": removed }),
        },
    )
    .await;
    crate::notify::waiting_settled(&state.db, ctx.wiki.id).await;
    Ok(pages::see_other("/drafts?withdrawn=1"))
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

    async fn user(db: &PgPool, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash) VALUES ($1, $2, $2 || '@example.test', 'x')",
        )
        .bind(id)
        .bind(name)
        .execute(db)
        .await
        .expect("user");
        id
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_bound_curator_checks_their_wards_and_maybe_nobodys(db: PgPool) {
        let (wiki, _, _) = page_with_text(&db, "x").await;
        let (curator, other, ward, theirs, loose) = (
            user(&db, "cur").await,
            user(&db, "cur2").await,
            user(&db, "ward").await,
            user(&db, "theirs").await,
            user(&db, "loose").await,
        );
        for (person, by) in [(ward, curator), (theirs, other)] {
            sqlx::query(
                "INSERT INTO curatorships (wiki_id, user_id, curator_id) VALUES ($1, $2, $3)",
            )
            .bind(wiki)
            .bind(person)
            .bind(by)
            .execute(&db)
            .await
            .expect("curatorship");
        }
        let check = |bound, author| checks_on(&db, wiki, bound, author);
        assert!(
            check(None, Some(theirs)).await.expect("all"),
            "unbound sees all"
        );
        let assigned = Some((curator, true));
        assert!(check(assigned, Some(ward)).await.expect("ward"));
        assert!(!check(assigned, Some(theirs)).await.expect("theirs"));
        assert!(
            check(assigned, Some(loose)).await.expect("loose"),
            "nobody's"
        );
        assert!(
            check(assigned, None).await.expect("guest"),
            "a guest is nobody's"
        );
        let only = Some((curator, false));
        assert!(check(only, Some(ward)).await.expect("ward"));
        assert!(!check(only, Some(loose)).await.expect("loose"));
        assert!(!check(only, None).await.expect("guest"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn only_its_author_takes_an_edit_back(db: PgPool) {
        let (wiki, page, rev) = page_with_text(&db, "accepted text").await;
        let alice = user(&db, "alice").await;
        let bob = user(&db, "bob").await;
        let mut conn = db.acquire().await.expect("conn");
        let pending = save_pending(
            &mut conn,
            page,
            Some(rev),
            Some(alice),
            "A",
            None,
            "alice's text",
            false,
        )
        .await
        .expect("pending");
        assert!(
            take_back(&mut conn, wiki, bob, pending, "x")
                .await
                .expect("q")
                .is_none(),
            "not his"
        );
        assert!(
            take_back(&mut conn, Uuid::new_v4(), alice, pending, "x")
                .await
                .expect("q")
                .is_none(),
            "not this wiki"
        );
        assert_eq!(
            take_back(&mut conn, wiki, alice, pending, "taken back")
                .await
                .expect("q"),
            Some((page, false)),
            "the page keeps its accepted text"
        );
        let status: String =
            sqlx::query_scalar("SELECT review_status FROM revisions WHERE id = $1")
                .bind(pending)
                .fetch_one(&db)
                .await
                .expect("status");
        assert_eq!(status, "rejected");
        assert!(
            take_back(&mut conn, wiki, alice, pending, "x")
                .await
                .expect("q")
                .is_none(),
            "once is enough"
        );

        // A new page that only ever waited goes with its edit.
        let fresh = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale) VALUES ($1, $2, 'main', 'b', 'B', 'en')",
        )
        .bind(fresh)
        .bind(wiki)
        .execute(&db)
        .await
        .expect("page");
        let first = save_pending(&mut conn, fresh, None, Some(alice), "B", None, "b", false)
            .await
            .expect("pending");
        assert_eq!(
            take_back(&mut conn, wiki, alice, first, "x")
                .await
                .expect("q"),
            Some((fresh, true))
        );
    }
}
