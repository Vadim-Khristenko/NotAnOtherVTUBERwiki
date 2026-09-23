//! Profiles at `/user/{name}`: account facts from the database plus a page
//! in the `user` namespace, edited like any article. An unwritten profile
//! invites its owner to write it; reserved former names redirect.

use axum::extract::{Extension, Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, FormView};
use crate::perm::WikiRole;
use crate::resolve::Ctx;

/// Recent edits listed on a profile.
const RECENT_EDITS: i64 = 8;

struct Person {
    id: Uuid,
    username: String,
    display_name: Option<String>,
    avatar_key: Option<String>,
    global_role: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

enum Lookup {
    Found(Person),
    /// A former name: redirect to the current one.
    Renamed(String),
    Missing,
}

async fn lookup(state: &AppState, name: &str) -> Result<Lookup, AppError> {
    let name = name.trim().to_lowercase();
    if let Some(row) = sqlx::query!(
        "SELECT id, username, display_name, avatar_key, global_role, created_at
         FROM users WHERE lower(username) = $1",
        name
    )
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(Lookup::Found(Person {
            id: row.id,
            username: row.username,
            display_name: row.display_name,
            avatar_key: row.avatar_key,
            global_role: row.global_role,
            created_at: row.created_at,
        }));
    }
    let policy = crate::policy::accounts(state).await?;
    if policy.aliases_disabled {
        return Ok(Lookup::Missing);
    }
    let current = sqlx::query_scalar!(
        "SELECT u.username FROM user_aliases a JOIN users u ON u.id = a.user_id
         WHERE lower(a.alias) = $1
           AND ($2::bigint = 0 OR a.created_at > now() - make_interval(days => $2::int))",
        name,
        policy.alias_days
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(current.map_or(Lookup::Missing, Lookup::Renamed))
}

/// The owner, their assigned curator (while still curator or above), or a
/// moderator and up.
async fn may_edit(state: &AppState, ctx: &Ctx, person: &Person) -> Result<bool, AppError> {
    if ctx.actor.user_id == Some(person.id) {
        return Ok(true);
    }
    let role = ctx.actor.effective_role();
    if role.is_some_and(|r| r >= WikiRole::Moderator) {
        return Ok(true);
    }
    if !role.is_some_and(|r| r >= WikiRole::Curator)
        || !ctx.actor.can(crate::perm::Capability::PageEdit)
    {
        return Ok(false);
    }
    let Some(me) = ctx.actor.user_id else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM curatorships
             WHERE wiki_id = $1 AND user_id = $2 AND curator_id = $3) AS "yes!""#,
        ctx.wiki.id,
        person.id,
        me
    )
    .fetch_one(&state.db)
    .await?)
}

struct ProfilePage {
    page_id: Uuid,
    revision_id: Uuid,
    body_md: String,
    locked: bool,
    protection: Option<WikiRole>,
}

async fn profile_page(
    state: &AppState,
    wiki_id: Uuid,
    username: &str,
) -> Result<Option<ProfilePage>, AppError> {
    let row = sqlx::query!(
        r#"SELECT p.id, p.is_locked, p.edit_level, r.id AS revision_id, r.body_md
           FROM pages p JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'user' AND p.slug = $2
             AND p.deleted_at IS NULL"#,
        wiki_id,
        username
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|row| ProfilePage {
        page_id: row.id,
        revision_id: row.revision_id,
        body_md: row.body_md,
        locked: row.is_locked,
        protection: pages::protection_of(row.is_locked, row.edit_level.as_deref()),
    }))
}

async fn resolve(
    state: &AppState,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
    name: &str,
    suffix: &str,
) -> Result<Result<(Ctx, Person), Response>, AppError> {
    let Some(ctx) = crate::resolve::context(state, headers, user).await? else {
        return Ok(Err(crate::errors::not_found()));
    };
    match lookup(state, name).await? {
        Lookup::Found(person) => Ok(Ok((ctx, person))),
        Lookup::Renamed(current) => Ok(Err(Redirect::permanent(&format!(
            "/user/{current}{suffix}"
        ))
        .into_response())),
        Lookup::Missing => Ok(Err(crate::errors::not_found())),
    }
}

/// GET /user/{name}
pub async fn show(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let (ctx, person) = or_respond!(resolve(&state, &headers, user.as_ref(), &name, "").await?);
    let page = profile_page(&state, ctx.wiki.id, &person.username).await?;
    let body_html = match &page {
        Some(page) => Some(pages::cached_body(&state, &ctx, "", &page.body_md).await?.0),
        None => None,
    };

    let role_here = sqlx::query_scalar!(
        r#"SELECT role::text AS "role!" FROM wiki_memberships WHERE user_id = $1 AND wiki_id = $2"#,
        person.id,
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?;
    let edits = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE r.author_id = $1 AND p.wiki_id = $2 AND p.namespace = 'main'"#,
        person.id,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?;
    let recent = sqlx::query!(
        r#"SELECT p.slug, p.title, r.created_at, r.summary
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE r.author_id = $1 AND p.wiki_id = $2 AND p.namespace = 'main'
             AND p.deleted_at IS NULL
           ORDER BY r.created_at DESC LIMIT $3"#,
        person.id,
        ctx.wiki.id,
        RECENT_EDITS
    )
    .fetch_all(&state.db)
    .await?;
    let recent: Vec<minijinja::Value> = recent
        .into_iter()
        .map(|row| {
            minijinja::context! {
                slug => row.slug,
                title => row.title,
                at => row.created_at.format("%Y-%m-%d").to_string(),
                summary => row.summary,
            }
        })
        .collect();

    let can_edit_profile = may_edit(&state, &ctx, &person).await?;
    let shown_name = person
        .display_name
        .clone()
        .unwrap_or_else(|| person.username.clone());
    let is_me = ctx.actor.user_id == Some(person.id);
    let extra = minijinja::context! {
        person_username => person.username.clone(),
        person_name => shown_name.clone(),
        person_has_display_name => person.display_name.is_some(),
        person_avatar => person.avatar_key.as_deref().map(crate::media::url_for_key),
        person_role => role_here,
        person_global => crate::perm::GlobalRole::parse(&person.global_role).as_str(),
        person_joined => person.created_at.format("%Y-%m-%d").to_string(),
        person_edits => edits,
        recent => recent,
        is_me => is_me,
        can_edit_profile => can_edit_profile,
        has_profile => body_html.is_some(),
    };
    let html = pages::render_shell(
        &ctx,
        &pages::Shell {
            title: &shown_name,
            body_html: body_html.as_deref().unwrap_or(""),
            render_ms: None,
            extra,
            template: "profile.html",
        },
    )?;
    Ok(pages::html_response(html, &headers))
}

/// GET /user/{name}/edit
pub async fn edit(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let (ctx, person) =
        or_respond!(resolve(&state, &headers, user.as_ref(), &name, "/edit").await?);
    if !may_edit(&state, &ctx, &person).await? {
        return Ok(forbidden(&ctx, &person));
    }
    let page = profile_page(&state, ctx.wiki.id, &person.username).await?;
    let shown = person
        .display_name
        .clone()
        .unwrap_or_else(|| person.username.clone());
    let heading = ctx.t_with("profile.editing", &[("name", &shown)]);
    let starter = ctx.t("profile.starter");
    pages::render_form(
        &ctx,
        &FormView {
            heading: &heading,
            action: &format!("/user/{}/edit", person.username),
            show_slug: false,
            slug: "",
            title_value: &shown,
            summary_value: "",
            body_md: page
                .as_ref()
                .map_or(starter.as_str(), |p| p.body_md.as_str()),
            base_revision: &page
                .as_ref()
                .map(|p| p.revision_id.to_string())
                .unwrap_or_default(),
            locked: page.as_ref().is_some_and(|p| p.locked),
            fixed_title: true,
            back_href: Some(&format!("/user/{}", person.username)),
            translation_of: None,
            form_locale: None,
        },
    )
}

fn forbidden(ctx: &Ctx, person: &Person) -> Response {
    pages::notice(
        ctx,
        StatusCode::FORBIDDEN,
        &ctx.t("error.not_allowed"),
        &ctx.t("profile.not_yours"),
        &format!("/user/{}", person.username),
        &ctx.t("profile.back"),
    )
    .unwrap_or_else(|err| err.into_response())
}

/// Somebody saved this profile between loading and saving.
fn conflict(ctx: &Ctx, person: &Person) -> Result<Response, AppError> {
    pages::notice(
        ctx,
        StatusCode::CONFLICT,
        &ctx.t("error.conflict_title"),
        &ctx.t("profile.conflict"),
        &format!("/user/{}/edit", person.username),
        &ctx.t("profile.back"),
    )
}

#[derive(serde::Deserialize)]
pub struct ProfileForm {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    body_md: String,
    #[serde(default)]
    base_revision: String,
}

/// POST /user/{name}/edit
pub async fn save(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(form): Form<ProfileForm>,
) -> Result<Response, AppError> {
    let (ctx, person) =
        or_respond!(resolve(&state, &headers, user.as_ref(), &name, "/edit").await?);
    if !may_edit(&state, &ctx, &person).await? {
        return Ok(forbidden(&ctx, &person));
    }
    // The title of a profile is the person's name.
    let mut draft = match pages::validate(&person.username, &form.summary, &form.body_md) {
        Ok(draft) => draft,
        Err(reason) => return Ok(pages::bad_request(reason)),
    };
    draft.body_md = crate::media::localize(&state, &ctx, draft.body_md).await;
    let existing = profile_page(&state, ctx.wiki.id, &person.username).await?;
    if let Some(page) = &existing {
        if !page
            .protection
            .is_none_or(|level| ctx.actor.effective_role().is_some_and(|r| r >= level))
        {
            return Ok(forbidden(&ctx, &person));
        }
        if let Some(base) = pages::parse_uuid(&form.base_revision)
            && base != page.revision_id
        {
            return conflict(&ctx, &person);
        }
        if page.body_md == draft.body_md {
            return Ok(pages::see_other(&format!("/user/{}", person.username)));
        }
    }

    let revision_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    let page_id = match &existing {
        Some(page) => page.page_id,
        None => {
            let id = Uuid::new_v4();
            // Profiles have no language. Two first saves racing: the unique index takes
            // one, the other is a conflict.
            match sqlx::query!(
                "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale)
                 VALUES ($1, $2, 'user', $3, $3, NULL)",
                id,
                ctx.wiki.id,
                person.username
            )
            .execute(&mut *tx)
            .await
            {
                Err(err) if pages::is_unique_violation(&err) => return conflict(&ctx, &person),
                result => result?,
            };
            id
        }
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
    // Compare and swap on the loaded revision (none for a just-created page).
    let swapped = sqlx::query!(
        "UPDATE pages SET current_revision_id = $1, updated_at = now()
         WHERE id = $2 AND current_revision_id IS NOT DISTINCT FROM $3",
        revision_id,
        page_id,
        existing.as_ref().map(|page| page.revision_id)
    )
    .execute(&mut *tx)
    .await?;
    if swapped.rows_affected() == 0 {
        return conflict(&ctx, &person);
    }
    tx.commit().await?;
    pages::after_save(&state, &ctx, page_id, "", &draft.body_md).await;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: if existing.is_some() {
                "profile.edit"
            } else {
                "profile.create"
            },
            entity_type: "page",
            entity_id: Some(page_id),
            meta: json!({ "profile_of": person.username, "revision": revision_id }),
        },
    )
    .await;
    Ok(pages::see_other(&format!("/user/{}", person.username)))
}
