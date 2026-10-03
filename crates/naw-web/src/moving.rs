//! Renaming an article: every language version moves to the new address at
//! once, its history with it, and the old address leads to the new one.
//!
//! Curators and up may move a page they may edit. Only articles move for
//! now: a template or a category is named in other pages' text, so moving it
//! needs those pages followed too.
//!
//! A redirect goes from an address to an address (`page_redirects`). Moving
//! a page also points the redirects that led to its old address at the new
//! one, so a chain never forms, and drops a redirect that sat on the new
//! address, which the page now takes.

use axum::extract::{Extension, Form, Path, State};
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

/// The address a redirect at `slug` leads to, when there is one.
pub(crate) async fn redirect_target(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    namespace: &str,
    slug: &str,
) -> Result<Option<String>, AppError> {
    let row = sqlx::query!(
        r#"SELECT to_namespace::text AS "to_namespace!", to_slug FROM page_redirects
           WHERE wiki_id = $1 AND namespace = ($2::text)::page_namespace AND from_slug = $3"#,
        wiki_id,
        namespace,
        slug
    )
    .fetch_optional(db)
    .await?;
    Ok(row.and_then(|r| pages::path_of(&r.to_namespace, &r.to_slug)))
}

/// Whether this reader may move this page.
pub(crate) fn may_move(ctx: &Ctx, path: &str, protection: Option<crate::perm::WikiRole>) -> bool {
    pages::split_path(path).0 == "main"
        && ctx.actor.can(Capability::RevisionPatrol)
        && ctx.actor.can_edit_page(protection)
}

#[derive(serde::Deserialize)]
pub struct MoveForm {
    to: String,
    #[serde(default)]
    reason: String,
    /// Unchecked boxes are absent from the post.
    #[serde(default)]
    redirect: Option<String>,
}

fn refuse(ctx: &Ctx, slug: &str, status: StatusCode, message: &str) -> Result<Response, AppError> {
    pages::notice(
        ctx,
        status,
        &ctx.t("move.refused"),
        message,
        &ctx.link(&format!("/{slug}")),
        &ctx.t("move.back"),
    )
}

/// Moves every language version of the article at `from` to `to`, inside
/// the caller's transaction; leaves a redirect when asked. `false` when
/// nothing was at `from`.
pub(crate) async fn move_article(
    conn: &mut sqlx::PgConnection,
    wiki_id: Uuid,
    from: &str,
    to: &str,
    leave_redirect: bool,
    by: Option<Uuid>,
) -> Result<bool, AppError> {
    let moved = sqlx::query!(
        "UPDATE pages SET slug = $3, updated_at = now()
         WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2",
        wiki_id,
        from,
        to
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if moved == 0 {
        return Ok(false);
    }
    // The page now owns the new address, and redirects to the old one follow it.
    sqlx::query!(
        "DELETE FROM page_redirects WHERE wiki_id = $1 AND namespace = 'main' AND from_slug = $2",
        wiki_id,
        to
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE page_redirects SET to_slug = $3
         WHERE wiki_id = $1 AND to_namespace = 'main' AND to_slug = $2",
        wiki_id,
        from,
        to
    )
    .execute(&mut *conn)
    .await?;
    if leave_redirect {
        sqlx::query!(
            "INSERT INTO page_redirects (wiki_id, namespace, from_slug, to_namespace, to_slug, created_by)
             VALUES ($1, 'main', $2, 'main', $3, $4)
             ON CONFLICT (wiki_id, namespace, from_slug)
             DO UPDATE SET to_namespace = 'main', to_slug = $3, created_by = $4, created_at = now()",
            wiki_id,
            from,
            to,
            by
        )
        .execute(&mut *conn)
        .await?;
    }
    // Drafts of edits follow the page they edit.
    sqlx::query!(
        "UPDATE drafts SET path = $3 WHERE wiki_id = $1 AND path = $2",
        wiki_id,
        from,
        to
    )
    .execute(&mut *conn)
    .await?;
    Ok(true)
}

/// POST /{slug}/move
pub async fn move_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<MoveForm>,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    if !pages::slug_is_valid(&slug) {
        return Ok(crate::errors::not_found());
    }
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(found) = pages::find_page(&state.db, ctx.wiki.id, &slug, &ctx.content_locale).await?
    else {
        return Ok(crate::errors::not_found());
    };
    if !may_move(&ctx, &slug, found.protection) {
        return refuse(
            &ctx,
            &slug,
            StatusCode::FORBIDDEN,
            &ctx.t("move.not_allowed"),
        );
    }
    let to = form.to.trim().trim_start_matches('/').to_lowercase();
    if to == slug {
        return Ok(pages::see_other(&ctx.link(&format!("/{slug}"))));
    }
    if !pages::slug_is_valid(&to) || pages::split_path(&to).0 != "main" {
        return refuse(
            &ctx,
            &slug,
            StatusCode::UNPROCESSABLE_ENTITY,
            &ctx.t("move.bad_address"),
        );
    }
    if let Some(key) = pages::reserved(&to, |code| ctx.skin.messages.has(code)) {
        return refuse(
            &ctx,
            &slug,
            StatusCode::UNPROCESSABLE_ENTITY,
            &ctx.t_with(key, &[("slug", &to)]),
        );
    }
    // Any version of any page there, archived ones included, keeps it taken.
    let taken = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2) AS "taken!""#,
        ctx.wiki.id,
        to
    )
    .fetch_one(&state.db)
    .await?;
    if taken {
        return refuse(
            &ctx,
            &slug,
            StatusCode::CONFLICT,
            &ctx.t_with("move.taken", &[("slug", &to)]),
        );
    }
    let leave_redirect = form.redirect.is_some();
    let reason: String = form
        .reason
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect();
    let mut tx = state.db.begin().await?;
    if !move_article(
        &mut tx,
        ctx.wiki.id,
        &slug,
        &to,
        leave_redirect,
        ctx.actor.user_id,
    )
    .await?
    {
        return Ok(crate::errors::not_found());
    }
    tx.commit().await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "page.move",
            entity_type: "page",
            entity_id: Some(found.id),
            meta: json!({ "from": slug, "to": to, "redirect": leave_redirect, "reason": reason }),
        },
    )
    .await;
    Ok(pages::see_other(&ctx.link(&format!("/{to}"))))
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::PgPool;

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

    async fn page(db: &PgPool, wiki_id: Uuid, slug: &str, locale: &str) {
        sqlx::query(
            "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale) VALUES ($1, $2, 'main', $3, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(wiki_id)
        .bind(slug)
        .bind(locale)
        .execute(db)
        .await
        .expect("page");
    }

    async fn slugs(db: &PgPool, wiki_id: Uuid) -> Vec<String> {
        sqlx::query_scalar("SELECT slug || '/' || locale FROM pages WHERE wiki_id = $1 ORDER BY 1")
            .bind(wiki_id)
            .fetch_all(db)
            .await
            .expect("slugs")
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn every_language_moves_and_the_old_address_leads_on(db: PgPool) {
        let here = wiki(&db, "here").await;
        let there = wiki(&db, "there").await;
        page(&db, here, "old", "en").await;
        page(&db, here, "old", "ru").await;
        page(&db, there, "old", "en").await;
        let mut conn = db.acquire().await.expect("conn");
        assert!(
            move_article(&mut conn, here, "old", "new", true, None)
                .await
                .expect("move")
        );
        assert_eq!(slugs(&db, here).await, ["new/en", "new/ru"]);
        assert_eq!(
            slugs(&db, there).await,
            ["old/en"],
            "the other wiki keeps its page"
        );
        assert_eq!(
            redirect_target(&db, here, "main", "old")
                .await
                .expect("target")
                .as_deref(),
            Some("new")
        );
        assert_eq!(
            redirect_target(&db, there, "main", "old")
                .await
                .expect("target"),
            None
        );

        // A second move points the first redirect at the end, and frees the
        // address the page left only as a redirect.
        assert!(
            move_article(&mut conn, here, "new", "newer", true, None)
                .await
                .expect("move")
        );
        assert_eq!(
            redirect_target(&db, here, "main", "old")
                .await
                .expect("target")
                .as_deref(),
            Some("newer"),
            "no chain"
        );
        assert!(
            move_article(&mut conn, here, "newer", "old", false, None)
                .await
                .expect("move back")
        );
        assert_eq!(
            redirect_target(&db, here, "main", "old")
                .await
                .expect("target"),
            None,
            "the page took its old address back"
        );
        assert!(
            !move_article(&mut conn, here, "missing", "x", true, None)
                .await
                .expect("nothing"),
            "nothing there, nothing moved"
        );
    }
}
