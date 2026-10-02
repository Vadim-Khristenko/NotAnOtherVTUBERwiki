//! Drafts kept on the server while somebody writes.
//!
//! The editor saves what is typed a couple of seconds after the typing stops,
//! so a closed tab, a crash or another device does not cost the text. An edit
//! draft is one per author, page and language, and the editor of that page
//! opens with it; a new page has no address yet, so each new-page draft has
//! its own id and opens from "My drafts". Saving the page clears its draft.
//! Drafts are private: only their author ever reads them.

use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, ENGINE_VERSION, template_error};
use crate::perm::Capability;
use crate::resolve::Ctx;

/// One draft as the editor and the list use it.
pub(crate) struct Draft {
    pub id: Uuid,
    pub path: String,
    pub locale: String,
    pub title: String,
    pub summary: String,
    pub body_md: String,
    pub base_revision_id: Option<Uuid>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct SaveForm {
    #[serde(default)]
    draft_id: String,
    kind: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    locale: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    body_md: String,
    #[serde(default)]
    base_revision: String,
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// POST /drafts/save: the editor's autosave. Answers `{ "id": ... }`.
pub async fn save(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<SaveForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "sign in first"));
    };
    let Some(ctx) = crate::resolve::context(&state, &headers, Some(&user)).await? else {
        return Ok(json_error(StatusCode::NOT_FOUND, "no such wiki"));
    };
    let kind = match form.kind.as_str() {
        "new" if ctx.actor.can(Capability::PageCreate) => "new",
        "edit" if ctx.actor.can(Capability::PageEdit) => "edit",
        "new" | "edit" => return Ok(json_error(StatusCode::FORBIDDEN, "not allowed")),
        _ => {
            return Ok(json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "kind: new or edit",
            ));
        }
    };
    let path = form.path.trim().to_lowercase();
    if path.chars().count() > 109 || (kind == "edit" && !pages::slug_is_valid(&path)) {
        return Ok(json_error(StatusCode::UNPROCESSABLE_ENTITY, "path"));
    }
    if form.body_md.len() > ctx.limits.page_bytes
        || form.title.chars().count() > 200
        || form.summary.chars().count() > 200
    {
        return Ok(json_error(StatusCode::PAYLOAD_TOO_LARGE, "too long"));
    }
    let locale = {
        let raw = form.locale.trim().to_ascii_lowercase();
        if ctx.offered_languages().contains(&raw) {
            raw
        } else {
            ctx.content_locale.clone()
        }
    };
    let base = pages::parse_uuid(&form.base_revision);

    let id = if kind == "edit" {
        let existing = sqlx::query_scalar!(
            "SELECT id FROM drafts
             WHERE user_id = $1 AND wiki_id = $2 AND kind = 'edit' AND locale = $3 AND path = $4",
            user.id,
            ctx.wiki.id,
            locale,
            path
        )
        .fetch_optional(&state.db)
        .await?;
        if existing.is_none() && count(&state, &ctx, user.id).await? >= ctx.limits.drafts_per_person
        {
            return Ok(json_error(StatusCode::CONFLICT, "too many drafts"));
        }
        sqlx::query_scalar!(
            "INSERT INTO drafts (id, wiki_id, user_id, kind, path, locale, title, summary, body_md, base_revision_id)
             VALUES ($1, $2, $3, 'edit', $4, $5, $6, $7, $8, $9)
             ON CONFLICT (user_id, wiki_id, locale, path) WHERE kind = 'edit'
             DO UPDATE SET title = EXCLUDED.title, summary = EXCLUDED.summary,
                           body_md = EXCLUDED.body_md, updated_at = now(),
                           base_revision_id = COALESCE(drafts.base_revision_id, EXCLUDED.base_revision_id)
             RETURNING id",
            Uuid::new_v4(),
            ctx.wiki.id,
            user.id,
            path,
            locale,
            form.title,
            form.summary,
            form.body_md,
            base
        )
        .fetch_one(&state.db)
        .await?
    } else {
        let updated = match pages::parse_uuid(&form.draft_id) {
            Some(id) => {
                sqlx::query_scalar!(
                "UPDATE drafts SET path = $4, locale = $5, title = $6, summary = $7, body_md = $8,
                                   updated_at = now()
                 WHERE id = $1 AND user_id = $2 AND wiki_id = $3 AND kind = 'new'
                 RETURNING id",
                id,
                user.id,
                ctx.wiki.id,
                path,
                locale,
                form.title,
                form.summary,
                form.body_md
            )
                .fetch_optional(&state.db)
                .await?
            }
            None => None,
        };
        match updated {
            Some(id) => id,
            None => {
                if count(&state, &ctx, user.id).await? >= ctx.limits.drafts_per_person {
                    return Ok(json_error(StatusCode::CONFLICT, "too many drafts"));
                }
                sqlx::query_scalar!(
                    "INSERT INTO drafts (id, wiki_id, user_id, kind, path, locale, title, summary, body_md)
                     VALUES ($1, $2, $3, 'new', $4, $5, $6, $7, $8)
                     RETURNING id",
                    Uuid::new_v4(),
                    ctx.wiki.id,
                    user.id,
                    path,
                    locale,
                    form.title,
                    form.summary,
                    form.body_md
                )
                .fetch_one(&state.db)
                .await?
            }
        }
    };
    Ok(Json(json!({ "id": id.to_string() })).into_response())
}

async fn count(state: &AppState, ctx: &Ctx, user_id: Uuid) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM drafts WHERE user_id = $1 AND wiki_id = $2"#,
        user_id,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?)
}

#[derive(Debug, Deserialize)]
pub struct DiscardForm {
    /// Where to go afterwards: a path on this site.
    #[serde(default)]
    next: String,
}

/// POST /drafts/{id}/delete: the author throws a draft away.
pub async fn discard(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    Form(form): Form<DiscardForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(Redirect::to("/login?next=%2Fdrafts").into_response());
    };
    if let Some(id) = pages::parse_uuid(&id) {
        sqlx::query!(
            "DELETE FROM drafts WHERE id = $1 AND user_id = $2",
            id,
            user.id
        )
        .execute(&state.db)
        .await?;
    }
    let next = pages::local_path(form.next.trim()).unwrap_or("/drafts");
    Ok(Redirect::to(next).into_response())
}

/// GET /drafts: the author's drafts in this wiki, newest first.
pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(Redirect::to("/login?next=%2Fdrafts").into_response());
    };
    let Some(ctx) = crate::resolve::context(&state, &headers, Some(&user)).await? else {
        return Ok(crate::errors::not_found());
    };
    let rows = sqlx::query!(
        "SELECT id, kind, path, locale, title, left(body_md, 400) AS \"head!\", updated_at
         FROM drafts WHERE user_id = $1 AND wiki_id = $2
         ORDER BY updated_at DESC LIMIT $3",
        user.id,
        ctx.wiki.id,
        ctx.limits.drafts_per_person
    )
    .fetch_all(&state.db)
    .await?;
    let drafts: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|r| {
            let continue_href = if r.kind == "edit" {
                ctx.link_for(&r.locale, &format!("/{}/edit", r.path))
            } else {
                ctx.link_for(&r.locale, &format!("/new?draft={}", r.id))
            };
            let title = if r.title.trim().is_empty() {
                ctx.t("drafts.untitled")
            } else {
                r.title.clone()
            };
            let excerpt: String = r.head.split_whitespace().collect::<Vec<_>>().join(" ");
            minijinja::context! {
                id => r.id.to_string(),
                kind => r.kind.clone(),
                kind_label => ctx.t(&format!("drafts.kind_{}", r.kind)),
                path => r.path,
                language => crate::translate::native_name(&ctx, &r.locale),
                title => title,
                excerpt => excerpt.chars().take(200).collect::<String>(),
                at => format!("{}, {} UTC", ctx.day(r.updated_at), r.updated_at.format("%H:%M")),
                continue_href => continue_href,
            }
        })
        .collect();
    let template = ctx
        .skin
        .env
        .get_template("drafts.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("drafts.title"),
                version => ENGINE_VERSION,
                drafts => drafts,
                drafts_max => ctx.limits.drafts_per_person,
            }
        })
        .map_err(template_error)?;
    Ok(pages::private_page(StatusCode::OK, html))
}

/// The author's draft of an edit to `path` in `locale`, if there is one.
pub(crate) async fn for_edit(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    user_id: Uuid,
    path: &str,
    locale: &str,
) -> Result<Option<Draft>, AppError> {
    let row = sqlx::query!(
        "SELECT id, path, locale, title, summary, body_md, base_revision_id, updated_at
         FROM drafts
         WHERE user_id = $1 AND wiki_id = $2 AND kind = 'edit' AND path = $3 AND locale = $4",
        user_id,
        wiki_id,
        path,
        locale
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Draft {
        id: r.id,
        path: r.path,
        locale: r.locale,
        title: r.title,
        summary: r.summary,
        body_md: r.body_md,
        base_revision_id: r.base_revision_id,
        updated_at: r.updated_at,
    }))
}

/// The author's new-page draft `id`, if it is theirs and in this wiki.
pub(crate) async fn new_by_id(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    user_id: Uuid,
    id: Uuid,
) -> Result<Option<Draft>, AppError> {
    let row = sqlx::query!(
        "SELECT id, path, locale, title, summary, body_md, base_revision_id, updated_at
         FROM drafts WHERE id = $1 AND user_id = $2 AND wiki_id = $3 AND kind = 'new'",
        id,
        user_id,
        wiki_id
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Draft {
        id: r.id,
        path: r.path,
        locale: r.locale,
        title: r.title,
        summary: r.summary,
        body_md: r.body_md,
        base_revision_id: r.base_revision_id,
        updated_at: r.updated_at,
    }))
}

/// Clears the drafts a save made pointless: the edit draft of the page, or
/// the new-page draft the form carried.
pub(crate) async fn clear(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    user_id: Uuid,
    edit: Option<(&str, &str)>,
    new_id: Option<Uuid>,
) {
    let result = async {
        if let Some((path, locale)) = edit {
            sqlx::query!(
                "DELETE FROM drafts WHERE user_id = $1 AND wiki_id = $2 AND kind = 'edit'
                   AND path = $3 AND locale = $4",
                user_id,
                wiki_id,
                path,
                locale
            )
            .execute(db)
            .await?;
        }
        if let Some(id) = new_id {
            sqlx::query!(
                "DELETE FROM drafts WHERE id = $1 AND user_id = $2",
                id,
                user_id
            )
            .execute(db)
            .await?;
        }
        Ok::<(), sqlx::Error>(())
    }
    .await;
    if let Err(err) = result {
        tracing::warn!(error = %err, "could not clear a saved draft");
    }
}

/// What the editor template needs about a draft it opened with.
pub(crate) fn notice(ctx: &Ctx, draft: &Draft, back: &str) -> minijinja::Value {
    minijinja::context! {
        id => draft.id.to_string(),
        at => format!("{}, {} UTC", ctx.day(draft.updated_at), draft.updated_at.format("%H:%M")),
        discard_action => format!("/drafts/{}/delete", draft.id),
        back => back,
    }
}

/// With a database: a draft is its author's alone, stays in its wiki, and is
/// one per page and language for an edit.
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

    async fn draft(
        db: &PgPool,
        wiki_id: Uuid,
        user_id: Uuid,
        kind: &str,
        path: &str,
        body: &str,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO drafts (id, wiki_id, user_id, kind, path, locale, body_md)
             VALUES ($1, $2, $3, $4, $5, 'en', $6)",
        )
        .bind(id)
        .bind(wiki_id)
        .bind(user_id)
        .bind(kind)
        .bind(path)
        .bind(body)
        .execute(db)
        .await
        .expect("draft");
        id
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_draft_is_its_authors_alone(db: PgPool) {
        let w = wiki(&db, "w").await;
        let alice = user(&db, "alice").await;
        let bob = user(&db, "bob").await;
        let edit = draft(&db, w, alice, "edit", "lore", "alice's words").await;
        let new = draft(&db, w, alice, "new", "", "a new page").await;
        let mine = for_edit(&db, w, alice, "lore", "en")
            .await
            .expect("q")
            .expect("found");
        assert_eq!(mine.id, edit);
        // another author gets nothing, not even with the id in hand
        assert!(
            for_edit(&db, w, bob, "lore", "en")
                .await
                .expect("q")
                .is_none()
        );
        assert!(new_by_id(&db, w, bob, new).await.expect("q").is_none());
        assert!(new_by_id(&db, w, alice, new).await.expect("q").is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_draft_stays_in_its_wiki(db: PgPool) {
        let a = wiki(&db, "a").await;
        let b = wiki(&db, "b").await;
        let alice = user(&db, "alice").await;
        let new = draft(&db, a, alice, "new", "", "in a").await;
        draft(&db, a, alice, "edit", "lore", "in a").await;
        assert!(
            for_edit(&db, b, alice, "lore", "en")
                .await
                .expect("q")
                .is_none()
        );
        assert!(new_by_id(&db, b, alice, new).await.expect("q").is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn an_edit_has_one_draft_and_saving_clears_it(db: PgPool) {
        let w = wiki(&db, "w").await;
        let alice = user(&db, "alice").await;
        draft(&db, w, alice, "edit", "lore", "first").await;
        let second = sqlx::query(
            "INSERT INTO drafts (id, wiki_id, user_id, kind, path, locale, body_md)
             VALUES ($1, $2, $3, 'edit', 'lore', 'en', 'second')",
        )
        .bind(Uuid::new_v4())
        .bind(w)
        .bind(alice)
        .execute(&db)
        .await;
        assert!(
            second.is_err(),
            "a second edit draft of the same page must not exist"
        );
        // new-page drafts may be many
        let one = draft(&db, w, alice, "new", "", "one").await;
        let two = draft(&db, w, alice, "new", "", "two").await;
        clear(&db, w, alice, Some(("lore", "en")), Some(one)).await;
        assert!(
            for_edit(&db, w, alice, "lore", "en")
                .await
                .expect("q")
                .is_none()
        );
        assert!(new_by_id(&db, w, alice, one).await.expect("q").is_none());
        assert!(new_by_id(&db, w, alice, two).await.expect("q").is_some());
    }
}
