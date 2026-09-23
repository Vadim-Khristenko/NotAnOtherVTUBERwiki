//! Page protection: the lowest role that may edit.
//!
//! Set by curators and up, never above their own role and never loosening a
//! level set above them (`Actor::may_protect`). `edit_level` holds the role;
//! `is_locked` stays true while any level is set, for older readers.

use axum::extract::{Extension, Form, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages;
use crate::perm::WikiRole;
use crate::resolve::Ctx;

/// The levels a page can be protected at, weakest first.
pub const LEVELS: [WikiRole; 4] = [
    WikiRole::Curator,
    WikiRole::Moderator,
    WikiRole::Admin,
    WikiRole::Owner,
];

/// The levels this actor may choose for a page at `current`; empty when they
/// may not change it.
pub fn choices(ctx: &Ctx, current: Option<WikiRole>) -> Vec<minijinja::Value> {
    if !ctx.actor.may_protect(current, None) {
        return Vec::new();
    }
    std::iter::once(None)
        .chain(LEVELS.into_iter().map(Some))
        .filter(|level| ctx.actor.may_protect(current, *level))
        .map(|level| {
            let id = level.map_or("none", WikiRole::as_str);
            minijinja::context! {
                id => id,
                label => ctx.t(&format!("page.protect_{id}")),
                current => level == current,
            }
        })
        .collect()
}

#[derive(serde::Deserialize)]
pub struct ProtectForm {
    /// A role name, or "none".
    level: String,
}

/// POST /{slug}/protect
pub async fn set(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<ProtectForm>,
) -> Result<Response, AppError> {
    let slug = slug.trim().to_lowercase();
    if !pages::slug_is_valid(&slug) {
        return Ok(crate::errors::not_found());
    }
    let Some(ctx) = crate::resolve::context(&state, &headers, user.as_ref()).await? else {
        return Ok(crate::errors::not_found());
    };
    let Some(found) = pages::find_page(&state.db, ctx.wiki.id, &slug, &ctx.content_locale).await?
    else {
        return Ok(crate::errors::not_found());
    };
    let wanted = match form.level.as_str() {
        "none" => None,
        raw => match WikiRole::parse(raw).filter(|r| LEVELS.contains(r)) {
            Some(level) => Some(level),
            None => {
                return Ok((
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "unknown level",
                )
                    .into_response());
            }
        },
    };
    if !ctx.actor.may_protect(found.protection, wanted) {
        return pages::notice(
            &ctx,
            axum::http::StatusCode::FORBIDDEN,
            &ctx.t("error.not_allowed"),
            &ctx.t("page.protect_refused"),
            &ctx.link(&format!("/{slug}")),
            &ctx.t("error.back_to_wiki"),
        );
    }
    sqlx::query!(
        "UPDATE pages SET edit_level = $2, is_locked = $3 WHERE id = $1",
        found.id,
        wanted.map(WikiRole::as_str),
        wanted.is_some()
    )
    .execute(&state.db)
    .await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "page.protect",
            entity_type: "page",
            entity_id: Some(found.id),
            meta: json!({
                "slug": slug,
                "locale": ctx.content_locale,
                "was": found.protection.map(WikiRole::as_str),
                "now": wanted.map(WikiRole::as_str),
            }),
        },
    )
    .await;
    Ok(pages::see_other(&ctx.link(&format!("/{slug}"))))
}
