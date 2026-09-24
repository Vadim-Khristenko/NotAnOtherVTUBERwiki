//! Special pages, as `/system:name`: lists and figures the wiki keeps about
//! itself rather than pages anyone writes. `/system` lists them all.

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::pages::{self, ENGINE_VERSION};
use crate::resolve::Ctx;

const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

/// The special pages, in the order `/system` lists them, with their icon.
const PAGES: [(&str, &str); 5] = [
    ("recent-changes", "activity"),
    ("all-pages", "list"),
    ("files", "photo"),
    ("statistics", "chart-bar"),
    ("random", "arrows-shuffle"),
];

/// Rows on one screen of a list.
const RECENT_MAX: i64 = 100;
const ALL_PAGES_MAX: i64 = 500;
const FILES_MAX: i64 = 120;

/// An edit this many bytes or more either way is shown in bold.
const BIG_EDIT_BYTES: i32 = 500;

/// GET /system and /system:name. `None` for a name that is not one.
pub(crate) async fn page(
    state: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    name: &str,
) -> Result<Option<Response>, AppError> {
    let (view, extra) = match name {
        "" => ("index", index(ctx)),
        "recent-changes" => ("recent-changes", recent_changes(state, ctx).await?),
        "all-pages" => ("all-pages", all_pages(state, ctx).await?),
        "files" => ("files", files(state, ctx).await?),
        "statistics" => ("statistics", statistics(state, ctx).await?),
        "random" => return random(state, ctx).await.map(Some),
        _ => return Ok(None),
    };
    let title = ctx.t(&format!("system.{}", view.replace('-', "_")));
    let template = ctx
        .skin
        .env
        .get_template("system.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => title.clone(),
                heading => title,
                version => ENGINE_VERSION,
                view => view,
            },
            ..extra,
        })
        .map_err(pages::template_error)?;
    // Lists change with every edit and carry nothing private.
    let _ = headers;
    Ok(Some(
        (
            StatusCode::OK,
            [HTML, (header::CACHE_CONTROL, "no-cache")],
            html,
        )
            .into_response(),
    ))
}

fn index(ctx: &Ctx) -> minijinja::Value {
    let entries: Vec<minijinja::Value> = PAGES
        .iter()
        .map(|(name, icon)| {
            let key = name.replace('-', "_");
            minijinja::context! {
                href => ctx.link(&format!("/system:{name}")),
                icon => icon,
                title => ctx.t(&format!("system.{key}")),
                about => ctx.t(&format!("system.{key}_about")),
                path => format!("System:{name}"),
            }
        })
        .collect();
    minijinja::context! { entries => entries }
}

/// Where a page of any namespace lives.
fn href_of(ctx: &Ctx, namespace: &str, slug: &str) -> String {
    match namespace {
        "user" => format!("/user/{slug}"),
        "template" => ctx.link(&format!("/template:{slug}")),
        "file" => ctx.link(&format!("/file:{slug}")),
        _ => ctx.link(&format!("/{slug}")),
    }
}

async fn recent_changes(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    // The previous revision of each page comes from a subquery, not a join,
    // so the query plan (and sqlx's reading of it) stays the same.
    let rows = sqlx::query!(
        r#"SELECT r.id, r.created_at, r.summary, r.is_minor, r.bytes AS "bytes!",
                  p.title, p.slug, p.namespace::text AS "namespace!",
                  COALESCE(p.locale, '') AS "locale!",
                  (SELECT u.username FROM users u WHERE u.id = r.author_id) AS "author?",
                  (SELECT r2.id FROM revisions r2 WHERE r2.page_id = r.page_id
                     AND (r2.created_at, r2.id) < (r.created_at, r.id)
                     ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1) AS "prev_id?",
                  (SELECT r2.bytes FROM revisions r2 WHERE r2.page_id = r.page_id
                     AND (r2.created_at, r2.id) < (r.created_at, r.id)
                     ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1) AS "prev_bytes?"
           FROM revisions r
           JOIN pages p ON p.id = r.page_id
           WHERE p.wiki_id = $1 AND p.deleted_at IS NULL
           ORDER BY r.created_at DESC, r.id DESC
           LIMIT $2"#,
        ctx.wiki.id,
        RECENT_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            let href = href_of(ctx, &row.namespace, &row.slug);
            let delta = row.bytes - row.prev_bytes.unwrap_or(0);
            // A diff lives under the page's own path, which for a profile is not here.
            let path = match row.namespace.as_str() {
                "user" => None,
                "template" => Some(format!("{}{}", pages::TEMPLATE_PREFIX, row.slug)),
                "file" => Some(format!("file:{}", row.slug)),
                _ => Some(row.slug.clone()),
            };
            let diff = match (&path, row.prev_id) {
                (Some(path), Some(prev)) => {
                    Some(ctx.link(&format!("/{path}/diff?from={prev}&to={}", row.id)))
                }
                _ => None,
            };
            let history = path.map(|path| ctx.link(&format!("/{path}/history")));
            minijinja::context! {
                title => row.title,
                href => href,
                diff => diff,
                history => history,
                author => row.author,
                summary => row.summary,
                is_minor => row.is_minor,
                is_new => row.prev_id.is_none(),
                delta => delta,
                delta_big => delta.abs() >= BIG_EDIT_BYTES,
                locale => (row.locale != ctx.content_locale).then_some(row.locale),
                day => ctx.day(row.created_at),
                time => row.created_at.format("%H:%M").to_string(),
            }
        })
        .collect();
    Ok(minijinja::context! { changes => items, recent_max => RECENT_MAX })
}

async fn all_pages(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
             AND COALESCE(p.locale, '') = $2
           ORDER BY lower(p.title), p.slug
           LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        ALL_PAGES_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let total = rows.len();
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            let letter = row
                .title
                .chars()
                .next()
                .map(|c| c.to_uppercase().collect::<String>())
                .unwrap_or_default();
            minijinja::context! {
                title => row.title,
                href => ctx.link(&format!("/{}", row.slug)),
                letter => letter,
            }
        })
        .collect();
    Ok(minijinja::context! { pages => items, total => total, all_pages_max => ALL_PAGES_MAX })
}

async fn files(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        "SELECT storage_key, name, kind, width, height FROM media
         WHERE wiki_id = $1 ORDER BY created_at DESC LIMIT $2",
        ctx.wiki.id,
        FILES_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            minijinja::context! {
                url => crate::media::url_for_key(&row.storage_key),
                page => ctx.link(&format!("/{}:{}", crate::files::prefix_of(&row.kind), row.name)),
                name => row.name,
                kind => row.kind,
                width => row.width,
                height => row.height,
            }
        })
        .collect();
    Ok(minijinja::context! { files => items })
}

async fn statistics(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let row = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL) AS "articles!",
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'template' AND deleted_at IS NULL) AS "templates!",
             (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id WHERE p.wiki_id = $1) AS "edits!",
             (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id
                WHERE p.wiki_id = $1 AND r.created_at > now() - interval '7 days') AS "edits_week!",
             (SELECT count(DISTINCT r.author_id) FROM revisions r JOIN pages p ON p.id = r.page_id
                WHERE p.wiki_id = $1 AND r.created_at > now() - interval '30 days') AS "editors_month!",
             (SELECT count(*) FROM wiki_memberships WHERE wiki_id = $1) AS "members!",
             (SELECT count(*) FROM media WHERE wiki_id = $1) AS "files!",
             (SELECT COALESCE(sum(size_bytes), 0)::bigint FROM media WHERE wiki_id = $1) AS "file_bytes!",
             (SELECT count(*) FROM emotes WHERE wiki_id = $1) AS "emotes!",
             (SELECT COALESCE(sum(octet_length(r.body_md)), 0)::bigint FROM pages p
                JOIN revisions r ON r.id = p.current_revision_id
                WHERE p.wiki_id = $1 AND p.deleted_at IS NULL) AS "text_bytes!""#,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?;
    let mb = |bytes: i64| format!("{:.1}", bytes as f64 / 1024.0 / 1024.0);
    let figures = [
        ("articles", row.articles.to_string()),
        ("templates", row.templates.to_string()),
        ("edits", row.edits.to_string()),
        ("edits_week", row.edits_week.to_string()),
        ("editors_month", row.editors_month.to_string()),
        ("members", row.members.to_string()),
        ("files", row.files.to_string()),
        ("file_mb", mb(row.file_bytes)),
        ("text_mb", mb(row.text_bytes)),
        ("emotes", row.emotes.to_string()),
    ];
    let items: Vec<minijinja::Value> = figures
        .into_iter()
        .map(|(key, value)| {
            minijinja::context! { label => ctx.t(&format!("system.stat_{key}")), value => value }
        })
        .collect();
    Ok(minijinja::context! { figures => items })
}

async fn random(state: &AppState, ctx: &Ctx) -> Result<Response, AppError> {
    let slug = sqlx::query_scalar!(
        "SELECT slug FROM pages
         WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL
           AND COALESCE(locale, '') = $2
         ORDER BY random() LIMIT 1",
        ctx.wiki.id,
        ctx.content_locale
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(match slug {
        Some(slug) => pages::see_other(&ctx.link(&format!("/{slug}"))),
        None => pages::see_other(&ctx.link("/")),
    })
}
