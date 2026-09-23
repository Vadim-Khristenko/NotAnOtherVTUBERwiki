//! Articles in several languages: one slug, one page row per language.
//!
//! Provides the interlanguage list, the staleness notice of a translation,
//! and the page for a language an article does not exist in yet, which
//! offers the other languages and, to editors, "translate".

use axum::extract::{Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, FormView};
use crate::perm::Capability;
use crate::resolve::Ctx;

/// One language an article exists in.
pub struct Version {
    pub locale: String,
    pub title: String,
}

/// Every language of the article at `slug`, the wiki's own first.
pub async fn versions(state: &AppState, ctx: &Ctx, slug: &str) -> Result<Vec<Version>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT COALESCE(locale, '') AS "locale!", title FROM pages
           WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2 AND deleted_at IS NULL
           ORDER BY (COALESCE(locale, '') = $3) DESC, locale"#,
        ctx.wiki.id,
        slug,
        ctx.wiki.default_locale
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Version {
            locale: row.locale,
            title: row.title,
        })
        .collect())
}

/// A language's name in itself.
pub fn native_name(ctx: &Ctx, locale: &str) -> String {
    ctx.skin
        .messages
        .meta(locale)
        .map(|m| m.native_name.clone())
        .unwrap_or_else(|| locale.to_uppercase())
}

/// The interlanguage list: every version but the one on screen.
pub fn others(ctx: &Ctx, slug: &str, all: &[Version]) -> Vec<minijinja::Value> {
    all.iter()
        .filter(|v| v.locale != ctx.content_locale)
        .map(|v| {
            minijinja::context! {
                code => v.locale.clone(),
                name => native_name(ctx, &v.locale),
                title => v.title.clone(),
                href => ctx.link_for(&v.locale, &format!("/{slug}")),
            }
        })
        .collect()
}

/// A translation whose source moved on since it was made or checked.
pub struct Stale {
    pub source_locale: String,
    pub source_href: String,
    pub diff_href: String,
}

/// Whether the page is a translation behind its source.
pub async fn staleness(
    state: &AppState,
    ctx: &Ctx,
    slug: &str,
    source_locale: Option<&str>,
    source_revision: Option<Uuid>,
) -> Result<Option<Stale>, AppError> {
    let Some(source_locale) = source_locale else {
        return Ok(None);
    };
    let current = sqlx::query_scalar!(
        r#"SELECT current_revision_id FROM pages
           WHERE wiki_id = $1 AND namespace = 'main' AND slug = $2
             AND COALESCE(locale, '') = $3 AND deleted_at IS NULL"#,
        ctx.wiki.id,
        slug,
        source_locale
    )
    .fetch_optional(&state.db)
    .await?
    .flatten();
    let Some(current) = current else {
        return Ok(None);
    };
    if source_revision == Some(current) {
        return Ok(None);
    }
    let diff_href = match source_revision {
        Some(from) => ctx.link_for(
            source_locale,
            &format!("/{slug}/diff?from={from}&to={current}"),
        ),
        None => ctx.link_for(source_locale, &format!("/{slug}/history")),
    };
    Ok(Some(Stale {
        source_locale: source_locale.to_string(),
        source_href: ctx.link_for(source_locale, &format!("/{slug}")),
        diff_href,
    }))
}

/// Where "translate into the reader's language" goes, when the article is
/// not in it yet and the reader may create pages.
pub fn translate_offer(ctx: &Ctx, slug: &str, all: &[Version]) -> Option<(String, String)> {
    if ctx.lang == ctx.content_locale
        || all.iter().any(|v| v.locale == ctx.lang)
        || !ctx.actor.can(Capability::PageCreate)
    {
        return None;
    }
    let href = format!(
        "{}?from={}",
        ctx.link_for(&ctx.lang, &format!("/{slug}/translate")),
        ctx.content_locale
    );
    Some((href, native_name(ctx, &ctx.lang)))
}

/// The page for an article missing in the requested language; `None` when
/// it exists nowhere.
pub async fn missing(
    state: &AppState,
    ctx: &Ctx,
    slug: &str,
) -> Result<Option<Response>, AppError> {
    let all = versions(state, ctx, slug).await?;
    let Some(source) = all.first() else {
        return Ok(None);
    };
    let translate_href = ctx.actor.can(Capability::PageCreate).then(|| {
        format!(
            "{}?from={}",
            ctx.link(&format!("/{slug}/translate")),
            source.locale
        )
    });
    let template = ctx
        .skin
        .env
        .get_template("untranslated.html")
        .map_err(pages::template_error)?;
    let here = native_name(ctx, &ctx.content_locale);
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => source.title.clone(),
                version => pages::ENGINE_VERSION,
                slug => slug,
                article_title => source.title.clone(),
                here_name => here,
                versions => others(ctx, slug, &all),
                translate_href => translate_href,
                source_name => native_name(ctx, &source.locale),
            }
        })
        .map_err(pages::template_error)?;
    // 404: nothing lives here yet, and the invitation should not be indexed.
    Ok(Some(
        (
            StatusCode::NOT_FOUND,
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
    ))
}

#[derive(serde::Deserialize)]
pub struct FromQuery {
    #[serde(default)]
    from: Option<String>,
}

/// The source: `?from=` when it exists, the wiki's language, then any.
async fn source_page(
    state: &AppState,
    ctx: &Ctx,
    slug: &str,
    from: Option<&str>,
) -> Result<Option<(String, pages::FoundPage)>, AppError> {
    let all = versions(state, ctx, slug).await?;
    let pick = from
        .and_then(|f| all.iter().find(|v| v.locale == f))
        .or_else(|| all.iter().find(|v| v.locale == ctx.wiki.default_locale))
        .or_else(|| all.iter().find(|v| v.locale != ctx.content_locale));
    let Some(pick) = pick.filter(|v| v.locale != ctx.content_locale) else {
        return Ok(None);
    };
    let locale = pick.locale.clone();
    Ok(pages::find_page(&state.db, ctx.wiki.id, slug, &locale)
        .await?
        .map(|page| (locale, page)))
}

async fn gate(
    state: &AppState,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
    slug: &str,
) -> Result<Result<Ctx, Response>, AppError> {
    let Some(ctx) = crate::resolve::context(state, headers, user).await? else {
        return Ok(Err(crate::errors::not_found()));
    };
    if !pages::slug_is_valid(slug) {
        return Ok(Err(crate::errors::not_found()));
    }
    if !ctx.actor.can(Capability::PageCreate) {
        let target = format!(
            "/login?next={}",
            crate::auth::redirect::encode_component(&ctx.link(&format!("/{slug}/translate")))
        );
        if !ctx.actor.is_signed_in() {
            return Ok(Err(pages::see_other(&target)));
        }
        return Ok(Err(pages::notice(
            &ctx,
            StatusCode::FORBIDDEN,
            &ctx.t("error.not_allowed"),
            &ctx.t("error.no_create"),
            &ctx.link(&format!("/{slug}")),
            &ctx.t("error.back_to_wiki"),
        )?));
    }
    // Already translated: edit that version instead.
    if pages::find_page(&state.db, ctx.wiki.id, slug, &ctx.content_locale)
        .await?
        .is_some()
    {
        return Ok(Err(pages::see_other(&ctx.link(&format!("/{slug}/edit")))));
    }
    Ok(Ok(ctx))
}

/// GET /{lang}/{slug}/translate?from=xx
pub async fn form(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    Query(query): Query<FromQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    let ctx = match gate(&state, &headers, user.as_ref(), &slug).await? {
        Ok(ctx) => ctx,
        Err(response) => return Ok(response),
    };
    let Some((from, source)) = source_page(&state, &ctx, &slug, query.from.as_deref()).await?
    else {
        return Ok(crate::errors::not_found());
    };
    let heading = ctx.t_with(
        "translate.heading",
        &[
            ("page", &source.title),
            ("from", &native_name(&ctx, &from)),
            ("to", &native_name(&ctx, &ctx.content_locale)),
        ],
    );
    let back = ctx.link_for(&from, &format!("/{slug}"));
    pages::render_form(
        &ctx,
        &FormView {
            heading: &heading,
            action: &format!("{}?from={from}", ctx.link(&format!("/{slug}/translate"))),
            show_slug: false,
            slug: "",
            title_value: &source.title,
            summary_value: "",
            body_md: &source.body_md,
            // The source revision translated from, stored so later changes show.
            base_revision: &source.revision_id.to_string(),
            locked: false,
            fixed_title: false,
            back_href: Some(&back),
            translation_of: None,
            form_locale: None,
        },
    )
}

#[derive(serde::Deserialize)]
pub struct TranslateForm {
    title: String,
    #[serde(default)]
    summary: String,
    body_md: String,
    #[serde(default)]
    base_revision: String,
}

/// POST /{lang}/{slug}/translate?from=xx
pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    Query(query): Query<FromQuery>,
    headers: HeaderMap,
    Form(form): Form<TranslateForm>,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    let ctx = match gate(&state, &headers, user.as_ref(), &slug).await? {
        Ok(ctx) => ctx,
        Err(response) => return Ok(response),
    };
    let Some((from, source)) = source_page(&state, &ctx, &slug, query.from.as_deref()).await?
    else {
        return Ok(crate::errors::not_found());
    };
    let draft = match pages::validate(&form.title, &form.summary, &form.body_md) {
        Ok(draft) => draft,
        Err(reason) => return Ok(pages::bad_request(reason)),
    };
    // The posted base revision if it belongs to the source, else the current one.
    let matched = match pages::parse_uuid(&form.base_revision) {
        Some(id) => sqlx::query_scalar!(
            "SELECT id FROM revisions WHERE id = $1 AND page_id = $2",
            id,
            source.id
        )
        .fetch_optional(&state.db)
        .await?
        .unwrap_or(source.revision_id),
        None => source.revision_id,
    };
    let locale = ctx.content_locale.clone();
    let page_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    // A translation that landed since the check gets a 409, not a 500.
    match sqlx::query!(
        "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale,
                            translation_source_locale, translation_source_revision_id)
         VALUES ($1, $2, 'main', $3, $4, $5, $6, $7)",
        page_id,
        ctx.wiki.id,
        slug,
        draft.title,
        locale,
        from,
        matched
    )
    .execute(&mut *tx)
    .await
    {
        Err(err) if pages::is_unique_violation(&err) => {
            return pages::slug_taken(&ctx, &slug, false);
        }
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
    pages::warm_cache(&state, ctx.wiki.id, &draft.body_md).await;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "page.translate",
            entity_type: "page",
            entity_id: Some(page_id),
            meta: json!({ "slug": slug, "from": from, "to": locale, "source_revision": matched }),
        },
    )
    .await;
    Ok(pages::see_other(&ctx.link(&format!("/{slug}"))))
}
