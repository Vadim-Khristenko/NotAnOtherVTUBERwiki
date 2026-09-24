//! Rebuilding the search index outside a save: for a whole wiki from the
//! command line or the admin panel, and for the pages that use a template
//! after that template changed.
//!
//! Each page is indexed the way a save indexes it: templates expanded, the
//! body rendered, and only its changed pieces rewritten.

use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;
use naw_markdown::transclude::Notes;

use crate::resolve::Ctx;
use crate::templates::{self, Wiki};

/// Re-indexes every live page of one wiki, or of every wiki when `wiki_id`
/// is `None`. Returns how many pages were indexed.
pub async fn reindex(db: &sqlx::PgPool, wiki_id: Option<Uuid>) -> Result<u64, AppError> {
    let ids = sqlx::query_scalar!(
        "SELECT id FROM pages
         WHERE deleted_at IS NULL AND current_revision_id IS NOT NULL
           AND ($1::uuid IS NULL OR wiki_id = $1)
         ORDER BY id",
        wiki_id
    )
    .fetch_all(db)
    .await?;
    let mut done = 0;
    for id in ids {
        if index_one(db, id).await? {
            done += 1;
        }
    }
    Ok(done)
}

/// Re-indexes, in the background, the pages that use the template `slug`:
/// they carry its text in their index. Failures are logged; the next save
/// or a reindex catches up.
pub(crate) fn refresh_users_of(state: &AppState, ctx: &Ctx, slug: &str) {
    let db = state.db.clone();
    let wiki_id = ctx.wiki.id;
    let slug = slug.to_string();
    tokio::spawn(async move {
        let pages = match sqlx::query_scalar!(
            "SELECT page_id FROM template_uses WHERE wiki_id = $1 AND template_slug = $2",
            wiki_id,
            slug
        )
        .fetch_all(&db)
        .await
        {
            Ok(pages) => pages,
            Err(err) => {
                tracing::warn!(error = %err, %slug, "could not list the pages of a template");
                return;
            }
        };
        let total = pages.len();
        for page_id in pages {
            if let Err(err) = index_one(&db, page_id).await {
                tracing::warn!(error = ?err, %page_id, "could not re-index a page after a template edit");
            }
        }
        tracing::info!(%slug, pages = total, "re-indexed the pages of a template");
    });
}

/// Indexes one live page and records the templates it uses. `false` when
/// the page is gone or archived.
async fn index_one(db: &sqlx::PgPool, page_id: Uuid) -> Result<bool, AppError> {
    let Some(row) = sqlx::query!(
        r#"SELECT p.wiki_id, p.namespace::text AS "namespace!", p.slug, p.title,
                  COALESCE(NULLIF(p.locale, ''), w.default_locale) AS "locale!",
                  w.default_locale, r.summary, r.body_md
           FROM pages p
           JOIN revisions r ON r.id = p.current_revision_id
           JOIN wikis w ON w.id = p.wiki_id
           WHERE p.id = $1 AND p.deleted_at IS NULL"#,
        page_id
    )
    .fetch_optional(db)
    .await?
    else {
        return Ok(false);
    };
    let path = match row.namespace.as_str() {
        "template" => format!("{}{}", crate::pages::TEMPLATE_PREFIX, row.slug),
        "file" => format!("file:{}", row.slug),
        "main" => row.slug.clone(),
        _ => String::new(),
    };
    let wiki = Wiki {
        id: row.wiki_id,
        locale: &row.locale,
        default_locale: &row.default_locale,
    };
    let notes = Notes {
        language: row.locale.clone(),
        ..Notes::default()
    };
    let expanded = templates::expand_in(db, &wiki, &notes, &path, &row.body_md).await?;
    let prepared = crate::pages::render_prepared(db, row.wiki_id, expanded).await?;
    let mut tx = db.begin().await?;
    naw_core::search::index_page(
        &mut tx,
        &naw_core::search::Document {
            page_id,
            wiki_id: row.wiki_id,
            locale: &row.locale,
            title: &row.title,
            summary: row.summary.as_deref(),
            html: &prepared.rendered.html,
        },
    )
    .await?;
    sqlx::query!("DELETE FROM template_uses WHERE page_id = $1", page_id)
        .execute(&mut *tx)
        .await?;
    if !prepared.used.is_empty() {
        sqlx::query!(
            "INSERT INTO template_uses (page_id, wiki_id, template_slug)
             SELECT $1, $2, unnest($3::text[]) ON CONFLICT DO NOTHING",
            page_id,
            row.wiki_id,
            &prepared.used
        )
        .execute(&mut *tx)
        .await?;
    }
    crate::files::record_uses(&mut tx, row.wiki_id, page_id, &prepared.rendered.html).await?;
    tx.commit().await?;
    Ok(true)
}
