//! The search page. Storage and querying live in `naw_core::search`.

use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tracing::instrument;

use naw_core::error::AppError;
use naw_core::search::{Postgres, Request, SearchBackend, normalize};
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, ENGINE_VERSION};

/// Results per page.
const LIMIT: i64 = 25;

const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

#[derive(Debug, serde::Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
}

/// GET /search
#[instrument(skip(state, user, headers))]
pub async fn search_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let normalized = normalize(&query.q);
    let hits = match normalized.as_deref() {
        Some(text) => {
            Postgres::new(state.db.clone())
                .search(Request {
                    wiki_id: ctx.wiki.id,
                    text,
                    locale: &ctx.content_locale,
                    limit: LIMIT,
                })
                .await?
        }
        None => Vec::new(),
    };
    let results: Vec<minijinja::Value> = hits
        .iter()
        .map(|hit| {
            minijinja::context! {
                slug => hit.slug.clone(),
                title => hit.title.clone(),
                section => hit.section.as_ref().map(|s| s.heading.clone()),
                anchor => hit.section.as_ref().map(|s| s.anchor.clone()),
                snippet => hit.snippet.clone(),
            }
        })
        .collect();
    let template = ctx
        .skin
        .env
        .get_template("search.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => match normalized.as_deref() {
                    Some(text) => ctx.t_with("search.title_for", &[("query", text)]),
                    None => ctx.t("search.heading"),
                },
                version => ENGINE_VERSION,
                query => query.q.clone(),
                searched => normalized.is_some(),
                results => results,
                result_count => results.len(),
                truncated => results.len() as i64 >= LIMIT,
            }
        })
        .map_err(pages::template_error)?;
    Ok((StatusCode::OK, [HTML], html).into_response())
}
