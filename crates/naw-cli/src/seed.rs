//! Seeds the first wiki and its first pages. Idempotent: when the snackers
//! wiki exists, this exits quietly with success and changes nothing.

use sqlx::PgPool;
use uuid::Uuid;

use naw_core::error::AppError;

const HOME_MD: &str = "# Welcome to SnackersWIKI\n\nThe fan wiki of the Snackers, the community around Filian. This wiki runs on the NotAnotherWiki engine: Markdown first, readable with JavaScript off, exportable in an open format.\n\nStart with [About](/about).\n";
const ABOUT_MD: &str = "# About SnackersWIKI\n\nSnackersWIKI documents Filian's streams, lore, community projects and inside jokes. Anyone in the community can propose an edit. Be kind, cite VODs, credit artists.\n";

pub async fn run(pool: &PgPool, skin_dir: &str) -> Result<(), AppError> {
    if sqlx::query!("SELECT id FROM wikis WHERE slug = 'snackers'")
        .fetch_optional(pool)
        .await?
        .is_some()
    {
        tracing::info!("seed: snackers already present, nothing to do");
        return Ok(());
    }
    let templates = naw_core::templates::load_templates(skin_dir)?;
    let wiki_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO wikis (id, slug, domain, name, settings) VALUES ($1, $2, $3, $4, $5)",
        wiki_id,
        "snackers",
        "snackers.vai-rice.space",
        "SnackersWIKI",
        serde_json::json!({"default": true, "home_slug": "home", "aliases": []})
    )
    .execute(pool)
    .await?;
    seed_page(
        pool,
        &templates,
        wiki_id,
        "SnackersWIKI",
        "home",
        "Home",
        HOME_MD,
    )
    .await?;
    seed_page(
        pool,
        &templates,
        wiki_id,
        "SnackersWIKI",
        "about",
        "About",
        ABOUT_MD,
    )
    .await?;
    tracing::info!("seed: snackers wiki with Home and About ready");
    Ok(())
}

async fn seed_page(
    pool: &PgPool,
    env: &minijinja::Environment<'_>,
    wiki_id: Uuid,
    wiki_name: &str,
    slug: &str,
    title: &str,
    body_md: &str,
) -> Result<(), AppError> {
    let rendered = naw_markdown::render_page(env, title, body_md, wiki_name, "en")?;
    let page_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale) VALUES ($1, $2, 'main', $3, $4, 'en')",
        page_id,
        wiki_id,
        slug,
        title
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        "INSERT INTO revisions (id, page_id, body_md, content_hash, summary) VALUES ($1, $2, $3, $4, $5)",
        revision_id,
        page_id,
        body_md,
        naw_markdown::content_hash(body_md),
        "seed"
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        "UPDATE pages SET current_revision_id = $1 WHERE id = $2",
        revision_id,
        page_id
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        "INSERT INTO render_cache (wiki_id, content_hash, renderer_version, html) VALUES ($1, $2, $3, $4)",
        wiki_id,
        rendered.content_hash,
        naw_markdown::RENDERER_VERSION,
        rendered.html
    )
    .execute(pool)
    .await?;
    Ok(())
}
