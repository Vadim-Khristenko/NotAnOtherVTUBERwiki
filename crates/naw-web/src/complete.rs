//! `/api/complete`: suggestions for the rich editor as people type `[[`,
//! `{{` or `[[Category:`. Only what any reader of the wiki can see already:
//! live pages, templates and categories in use.

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;

/// Suggestions in one answer.
const SHOWN: i64 = 20;

#[derive(Deserialize)]
pub struct CompleteQuery {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    q: String,
}

#[derive(Serialize)]
struct Item {
    label: String,
    detail: Option<String>,
    apply: String,
}

/// `q` as an ILIKE pattern piece: the wildcards it holds match themselves.
fn like(q: &str) -> String {
    q.trim()
        .chars()
        .take(100)
        .flat_map(|c| match c {
            '%' | '_' | '\\' => vec!['\\', c],
            c => vec![c],
        })
        .collect()
}

/// GET /api/complete?kind=page|template|category&q=
pub async fn complete(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Query(query): Query<CompleteQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let q = like(&query.q);
    let starts = format!("{q}%");
    let anywhere = format!("%{q}%");
    let items: Vec<Item> = match query.kind.as_str() {
        "page" | "template" => {
            let namespace = if query.kind == "page" {
                "main"
            } else {
                "template"
            };
            sqlx::query!(
                r#"SELECT DISTINCT ON (slug) slug, title FROM pages
                   WHERE wiki_id = $1 AND namespace = ($2::text)::page_namespace
                     AND deleted_at IS NULL AND current_revision_id IS NOT NULL
                     AND (slug ILIKE $3 OR title ILIKE $4)
                   ORDER BY slug, (COALESCE(locale, '') = $5) DESC
                   LIMIT $6"#,
                ctx.wiki.id,
                namespace,
                starts,
                anywhere,
                ctx.content_locale,
                SHOWN
            )
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .map(|row| Item {
                label: row.slug.clone(),
                detail: (row.title != row.slug).then_some(row.title),
                apply: row.slug,
            })
            .collect()
        }
        "category" => sqlx::query!(
            r#"SELECT category, min(name) AS "name!", count(*) AS "pages!"
               FROM page_categories
               WHERE wiki_id = $1 AND (category ILIKE $2 OR name ILIKE $3)
               GROUP BY category ORDER BY count(*) DESC, category
               LIMIT $4"#,
            ctx.wiki.id,
            starts,
            anywhere,
            SHOWN
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|row| Item {
            label: row.name.clone(),
            detail: Some(format!("{} · {}", row.category, row.pages)),
            apply: row.name,
        })
        .collect(),
        _ => {
            return Ok(
                (StatusCode::BAD_REQUEST, "kind: page, template or category").into_response(),
            );
        }
    };
    Ok((
        [(header::CACHE_CONTROL, "private, max-age=30")],
        Json(items),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_in_the_query_match_themselves() {
        assert_eq!(like(" 50%_off "), "50\\%\\_off");
        assert_eq!(like("Filian"), "Filian");
    }
}
