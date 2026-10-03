//! The watchlist and notifications.
//!
//! A signed-in reader watches a page by its address (`watches`), so the watch
//! covers every language version and follows a rename. Writing a page
//! watches it for its author. When somebody else changes a watched page,
//! each watcher gets a notification; further edits to the same page and
//! language, before the watcher has looked, refresh that one entry instead
//! of piling up.
//!
//! A notification also tells an author how review went for their edit, and
//! a reader that the staff answered their report. The header bell counts the
//! unread ones; `/notifications` lists them and marks them read, and
//! `/watchlist` lists the watched pages by their latest change.
//!
//! Notifications never fail the action behind them: a save that went through
//! is not turned into an error because a watcher could not be told.

use axum::extract::{Extension, Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, ENGINE_VERSION, template_error};
use crate::resolve::Ctx;

/// How many notifications the list shows.
const SHOWN: i64 = 100;

/// Read notifications older than this go when their owner opens the list.
const KEEP_READ_DAYS: i32 = 90;

/// The reader's unread notifications in this wiki. Asked on every page a
/// signed-in reader opens, and nearly always zero.
pub(crate) async fn unread_count(
    db: &sqlx::PgPool,
    user_id: Uuid,
    wiki_id: Uuid,
) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM notifications
           WHERE user_id = $1 AND wiki_id = $2 AND read_at IS NULL"#,
        user_id,
        wiki_id
    )
    .fetch_one(db)
    .await?)
}

/// Whether the reader watches the page at `path`.
pub(crate) async fn is_watching(
    db: &sqlx::PgPool,
    user_id: Uuid,
    wiki_id: Uuid,
    path: &str,
) -> Result<bool, AppError> {
    let (namespace, slug) = pages::split_path(path);
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM watches
             WHERE user_id = $1 AND wiki_id = $2 AND namespace = ($3::text)::page_namespace AND slug = $4
           ) AS "watching!""#,
        user_id,
        wiki_id,
        namespace,
        slug
    )
    .fetch_one(db)
    .await?)
}

/// Starts or stops watching the page at `path`.
pub(crate) async fn set_watch(
    db: &sqlx::PgPool,
    user_id: Uuid,
    wiki_id: Uuid,
    path: &str,
    on: bool,
) -> Result<(), sqlx::Error> {
    let (namespace, slug) = pages::split_path(path);
    if on {
        sqlx::query!(
            "INSERT INTO watches (user_id, wiki_id, namespace, slug)
             VALUES ($1, $2, ($3::text)::page_namespace, $4)
             ON CONFLICT DO NOTHING",
            user_id,
            wiki_id,
            namespace,
            slug
        )
        .execute(db)
        .await?;
    } else {
        sqlx::query!(
            "DELETE FROM watches
             WHERE user_id = $1 AND wiki_id = $2 AND namespace = ($3::text)::page_namespace AND slug = $4",
            user_id,
            wiki_id,
            namespace,
            slug
        )
        .execute(db)
        .await?;
    }
    Ok(())
}

/// Watches the page for the person who just wrote it.
pub(crate) async fn watch_written(db: &sqlx::PgPool, ctx: &Ctx, path: &str) {
    let Some(user_id) = ctx.actor.user_id else {
        return;
    };
    if let Err(err) = set_watch(db, user_id, ctx.wiki.id, path, true).await {
        tracing::warn!(error = %err, "could not watch a page for its author");
    }
}

/// Moves the watches of an article along with it, inside the caller's
/// transaction. Somebody who watched both addresses keeps one watch.
pub(crate) async fn move_watches(
    conn: &mut sqlx::PgConnection,
    wiki_id: Uuid,
    from: &str,
    to: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE watches w SET slug = $3
         WHERE w.wiki_id = $1 AND w.namespace = 'main' AND w.slug = $2
           AND NOT EXISTS (SELECT 1 FROM watches o WHERE o.user_id = w.user_id
                             AND o.wiki_id = $1 AND o.namespace = 'main' AND o.slug = $3)",
        wiki_id,
        from,
        to
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "DELETE FROM watches WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2",
        wiki_id,
        from
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A notification for one person.
pub(crate) struct Note<'a> {
    pub kind: &'static str,
    pub actor_id: Option<Uuid>,
    pub title: &'a str,
    pub link: &'a str,
    pub note: Option<&'a str>,
}

/// Leaves `note` for `user_id`. Nobody is told about their own action.
pub(crate) async fn push(db: &sqlx::PgPool, user_id: Uuid, wiki_id: Uuid, note: &Note<'_>) {
    if note.actor_id == Some(user_id) {
        return;
    }
    let result = sqlx::query!(
        "INSERT INTO notifications (id, user_id, wiki_id, kind, actor_id, title, link, note)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        Uuid::new_v4(),
        user_id,
        wiki_id,
        note.kind,
        note.actor_id,
        note.title,
        note.link,
        note.note.map(clip)
    )
    .execute(db)
    .await;
    if let Err(err) = result {
        tracing::warn!(error = %err, kind = note.kind, "could not leave a notification");
    }
}

/// An edit summary or an answer as a notification keeps it.
fn clip(text: &str) -> String {
    text.trim().chars().take(300).collect()
}

/// Tells the watchers of the page at `path` that it changed. `actor` made
/// the change; `skip` are more people who know already, like the curator
/// who accepted it. `link` is where to look, and an unread entry for the
/// same link is refreshed rather than repeated.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn page_edited(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    path: &str,
    actor: Option<Uuid>,
    skip: Option<Uuid>,
    title: &str,
    link: &str,
    summary: Option<&str>,
) {
    let (namespace, slug) = pages::split_path(path);
    let skip: Vec<Uuid> = actor.into_iter().chain(skip).collect();
    let summary = summary.map(clip).filter(|s| !s.is_empty());
    let result = sqlx::query!(
        "WITH watchers AS (
           SELECT user_id FROM watches
           WHERE wiki_id = $1 AND namespace = ($2::text)::page_namespace AND slug = $3
             AND user_id <> ALL($4::uuid[])
         ), refreshed AS (
           UPDATE notifications n SET actor_id = $5, title = $6, note = $8, created_at = now()
           FROM watchers w
           WHERE n.user_id = w.user_id AND n.wiki_id = $1 AND n.kind = 'page_edited'
             AND n.link = $7 AND n.read_at IS NULL
           RETURNING n.user_id
         )
         INSERT INTO notifications (id, user_id, wiki_id, kind, actor_id, title, link, note)
         SELECT gen_random_uuid(), w.user_id, $1, 'page_edited', $5, $6, $7, $8
         FROM watchers w
         WHERE w.user_id NOT IN (SELECT user_id FROM refreshed)",
        wiki_id,
        namespace,
        slug,
        &skip,
        actor,
        title,
        link,
        summary
    )
    .execute(db)
    .await;
    if let Err(err) = result {
        tracing::warn!(error = %err, "could not tell the watchers of a page");
    }
}

/// What a watcher opens for a change: the page's history in that language.
pub(crate) fn history_link(ctx: &Ctx, locale: &str, path: &str) -> String {
    ctx.link_for(locale, &format!("/{path}/history"))
}

#[derive(Debug, serde::Deserialize)]
pub struct WatchForm {
    /// "1" to watch, anything else to stop.
    #[serde(default)]
    on: String,
    /// Where to go afterwards: a path on this site.
    #[serde(default)]
    next: String,
}

/// POST /{slug}/watch
pub async fn watch(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<WatchForm>,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    if !pages::slug_is_valid(&slug) {
        return Ok(crate::errors::not_found());
    }
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(user_id) = ctx.actor.user_id else {
        let back = ctx.link(&format!("/{slug}"));
        return Ok(pages::see_other(&format!(
            "/login?next={}",
            pages::urlencode(&back)
        )));
    };
    let on = form.on == "1";
    // Watching needs a page there; stopping works for one that is gone.
    if on
        && pages::find_page(&state.db, ctx.wiki.id, &slug, &ctx.content_locale)
            .await?
            .is_none()
    {
        return Ok(crate::errors::not_found());
    }
    set_watch(&state.db, user_id, ctx.wiki.id, &slug, on).await?;
    let next = pages::local_path(form.next.trim())
        .map(str::to_string)
        .unwrap_or_else(|| ctx.link(&format!("/{slug}")));
    Ok(pages::see_other(&next))
}

/// GET /notifications: the reader's notifications here, newest first. Opening
/// the list reads them.
pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(Redirect::to("/login?next=%2Fnotifications").into_response());
    };
    let Some(mut ctx) = crate::resolve::context(&state, &headers, Some(&user)).await? else {
        return Ok(crate::errors::not_found());
    };
    sqlx::query!(
        "DELETE FROM notifications
         WHERE user_id = $1 AND wiki_id = $2 AND read_at < now() - make_interval(days => $3)",
        user.id,
        ctx.wiki.id,
        KEEP_READ_DAYS
    )
    .execute(&state.db)
    .await?;
    let rows = sqlx::query!(
        r#"SELECT n.kind, n.title, n.link, n.note, n.created_at, n.read_at IS NULL AS "unread!",
                  (SELECT u.username FROM users u WHERE u.id = n.actor_id) AS "actor?"
           FROM notifications n
           WHERE n.user_id = $1 AND n.wiki_id = $2
           ORDER BY n.created_at DESC
           LIMIT $3"#,
        user.id,
        ctx.wiki.id,
        SHOWN
    )
    .fetch_all(&state.db)
    .await?;
    sqlx::query!(
        "UPDATE notifications SET read_at = now()
         WHERE user_id = $1 AND wiki_id = $2 AND read_at IS NULL",
        user.id,
        ctx.wiki.id
    )
    .execute(&state.db)
    .await?;
    ctx.unread = 0;
    let anonymous = ctx.t("history.anonymous");
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|r| {
            let who = r.actor.clone().unwrap_or_else(|| anonymous.clone());
            minijinja::context! {
                kind => r.kind.clone(),
                text => ctx.t_with(&format!("notify.kind_{}", r.kind), &[("who", &who), ("title", &r.title)]),
                actor => r.actor,
                href => pages::local_path(&r.link).map(str::to_string),
                note => r.note,
                unread => r.unread,
                at => format!("{}, {} UTC", ctx.day(r.created_at), r.created_at.format("%H:%M")),
            }
        })
        .collect();
    render(
        &ctx,
        "notifications.html",
        &ctx.t("notify.title"),
        minijinja::context! { items => items, shown => SHOWN },
    )
}

/// GET /watchlist: the pages the reader watches, by their latest change.
pub async fn watchlist(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(Redirect::to("/login?next=%2Fwatchlist").into_response());
    };
    let Some(ctx) = crate::resolve::context(&state, &headers, Some(&user)).await? else {
        return Ok(crate::errors::not_found());
    };
    let shown = ctx.limits.system_list_shown;
    let rows = sqlx::query!(
        r#"SELECT w.namespace::text AS "namespace!", w.slug,
                  p.title AS "title?", p.locale AS "locale?",
                  r.created_at AS "changed_at?", r.summary AS "summary?", r.is_minor AS "minor?",
                  (SELECT u.username FROM users u WHERE u.id = r.author_id) AS "author?"
           FROM watches w
           LEFT JOIN pages p ON p.wiki_id = w.wiki_id AND p.namespace = w.namespace AND p.slug = w.slug
                            AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
           LEFT JOIN revisions r ON r.id = p.current_revision_id
           WHERE w.user_id = $1 AND w.wiki_id = $2
           ORDER BY r.created_at DESC NULLS LAST, w.slug
           LIMIT $3"#,
        user.id,
        ctx.wiki.id,
        shown
    )
    .fetch_all(&state.db)
    .await?;
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM watches WHERE user_id = $1 AND wiki_id = $2"#,
        user.id,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?;
    let anonymous = ctx.t("history.anonymous");
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .filter_map(|r| {
            let path = pages::path_of(&r.namespace, &r.slug)?;
            let locale = r.locale.clone().unwrap_or_default();
            let href = if locale.is_empty() {
                ctx.link(&format!("/{path}"))
            } else {
                ctx.link_for(&locale, &format!("/{path}"))
            };
            Some(minijinja::context! {
                path => path.clone(),
                title => r.title.clone().unwrap_or_else(|| path.clone()),
                exists => r.title.is_some(),
                href => href,
                history_href => (!locale.is_empty()).then(|| history_link(&ctx, &locale, &path)),
                language => (!locale.is_empty() && locale != ctx.content_locale)
                    .then(|| crate::translate::native_name(&ctx, &locale)),
                author => r.changed_at.map(|_| r.author.clone().unwrap_or_else(|| anonymous.clone())),
                summary => r.summary.filter(|s| !s.trim().is_empty()),
                minor => r.minor.unwrap_or(false),
                at => r.changed_at.map(|t| format!("{}, {} UTC", ctx.day(t), t.format("%H:%M"))),
                unwatch_action => format!("/{path}/watch"),
            })
        })
        .collect();
    render(
        &ctx,
        "watchlist.html",
        &ctx.t("notify.watchlist_title"),
        minijinja::context! { items => items, total => total, shown => shown },
    )
}

fn render(
    ctx: &Ctx,
    template: &str,
    title: &str,
    extra: minijinja::Value,
) -> Result<Response, AppError> {
    let html = ctx
        .skin
        .env
        .get_template(template)
        .map_err(template_error)?
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! { title => title, version => ENGINE_VERSION },
            ..extra
        })
        .map_err(template_error)?;
    Ok(pages::private_page(StatusCode::OK, html))
}

/// With a database: who hears about a change, and what they see.
#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::PgPool;

    /// The kinds of notification, as the table's check spells them.
    const KINDS: [&str; 4] = [
        "page_edited",
        "edit_accepted",
        "edit_rejected",
        "report_answered",
    ];

    #[test]
    fn every_kind_reads_in_every_language() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales");
        for lang in ["en", "ru"] {
            let text = std::fs::read_to_string(dir.join(lang).join("notify.toml")).expect("locale");
            for kind in KINDS {
                assert!(
                    text.contains(&format!("kind_{kind} = ")),
                    "{lang} has no line for {kind}"
                );
            }
        }
    }

    async fn wiki(db: &PgPool, slug: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO wikis (id, slug, name) VALUES ($1, $2, $2)")
            .bind(id)
            .bind(slug)
            .execute(db)
            .await
            .expect("wiki");
        id
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

    async fn notes(db: &PgPool, user_id: Uuid) -> Vec<(String, Option<String>)> {
        sqlx::query_as(
            "SELECT kind, note FROM notifications WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(db)
        .await
        .expect("notes")
    }

    /// The header asks on every page; with nothing there it must read zero,
    /// not fail.
    #[sqlx::test(migrations = "../../migrations")]
    async fn nothing_unread_reads_as_zero(db: PgPool) {
        let w = wiki(&db, "w").await;
        let alice = user(&db, "alice").await;
        assert_eq!(unread_count(&db, alice, w).await.expect("count"), 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn watchers_hear_of_an_edit_once_until_they_look(db: PgPool) {
        let w = wiki(&db, "w").await;
        let other = wiki(&db, "other").await;
        let alice = user(&db, "alice").await;
        let bob = user(&db, "bob").await;
        let carol = user(&db, "carol").await;
        set_watch(&db, alice, w, "lore", true).await.expect("watch");
        set_watch(&db, bob, w, "lore", true).await.expect("watch");
        set_watch(&db, carol, other, "lore", true)
            .await
            .expect("watch");
        assert!(is_watching(&db, alice, w, "lore").await.expect("q"));
        assert!(!is_watching(&db, alice, other, "lore").await.expect("q"));

        page_edited(
            &db,
            w,
            "lore",
            Some(bob),
            None,
            "Lore",
            "/lore/history",
            Some("first"),
        )
        .await;
        assert_eq!(
            notes(&db, alice).await,
            [("page_edited".to_string(), Some("first".to_string()))]
        );
        assert!(
            notes(&db, bob).await.is_empty(),
            "nobody hears of their own edit"
        );
        assert!(
            notes(&db, carol).await.is_empty(),
            "the other wiki's page is another page"
        );

        // A second edit before Alice looked refreshes the entry.
        page_edited(
            &db,
            w,
            "lore",
            Some(bob),
            None,
            "Lore",
            "/lore/history",
            Some("second"),
        )
        .await;
        assert_eq!(
            notes(&db, alice).await,
            [("page_edited".to_string(), Some("second".to_string()))]
        );
        assert_eq!(unread_count(&db, alice, w).await.expect("count"), 1);

        // Once read, the next edit is news again.
        sqlx::query("UPDATE notifications SET read_at = now() WHERE user_id = $1")
            .bind(alice)
            .execute(&db)
            .await
            .expect("read");
        page_edited(
            &db,
            w,
            "lore",
            None,
            Some(alice),
            "Lore",
            "/lore/history",
            None,
        )
        .await;
        assert_eq!(notes(&db, alice).await.len(), 1, "skipped: she accepted it");
        page_edited(&db, w, "lore", None, None, "Lore", "/lore/history", None).await;
        assert_eq!(notes(&db, alice).await.len(), 2);
        assert_eq!(unread_count(&db, alice, w).await.expect("count"), 1);

        set_watch(&db, alice, w, "lore", false)
            .await
            .expect("unwatch");
        assert!(!is_watching(&db, alice, w, "lore").await.expect("q"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_watch_follows_the_page_to_its_new_address(db: PgPool) {
        let w = wiki(&db, "w").await;
        let alice = user(&db, "alice").await;
        let bob = user(&db, "bob").await;
        set_watch(&db, alice, w, "old", true).await.expect("watch");
        set_watch(&db, bob, w, "old", true).await.expect("watch");
        set_watch(&db, bob, w, "new", true).await.expect("watch");
        let mut conn = db.acquire().await.expect("conn");
        move_watches(&mut conn, w, "old", "new")
            .await
            .expect("move");
        assert!(is_watching(&db, alice, w, "new").await.expect("q"));
        assert!(is_watching(&db, bob, w, "new").await.expect("q"));
        assert!(!is_watching(&db, alice, w, "old").await.expect("q"));
        assert!(!is_watching(&db, bob, w, "old").await.expect("q"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_note_never_goes_to_its_own_actor(db: PgPool) {
        let w = wiki(&db, "w").await;
        let alice = user(&db, "alice").await;
        let note = Note {
            kind: "edit_accepted",
            actor_id: Some(alice),
            title: "Lore",
            link: "/lore",
            note: None,
        };
        push(&db, alice, w, &note).await;
        assert!(notes(&db, alice).await.is_empty());
        let note = Note {
            actor_id: None,
            ..note
        };
        push(&db, alice, w, &note).await;
        assert_eq!(notes(&db, alice).await.len(), 1);
        for kind in KINDS {
            push(&db, alice, w, &Note { kind, ..note }).await;
        }
        assert_eq!(
            notes(&db, alice).await.len(),
            1 + KINDS.len(),
            "every kind passes the check"
        );
    }
}
