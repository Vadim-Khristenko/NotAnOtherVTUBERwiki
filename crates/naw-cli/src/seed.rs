//! Seeds the first wiki and its first pages. Idempotent: when the snackers
//! wiki exists, this exits quietly with success and changes nothing.

use sqlx::PgPool;
use uuid::Uuid;

use naw_core::error::AppError;

const HOME_MD: &str = "# Welcome to SnackersWIKI\n\nThe fan wiki of the Snackers, the community around Filian. This wiki runs on the NotAnotherWiki engine: Markdown first, readable with JavaScript off, exportable in an open format.\n\nStart with [About](/about).\n";
const ABOUT_MD: &str = "# About SnackersWIKI\n\nSnackersWIKI documents Filian's streams, lore, community projects and inside jokes. Anyone in the community can propose an edit. Be kind, cite VODs, credit artists.\n";
const PRIVACY_MD: &str = "# Privacy\n\nNo trackers. No ads. No analytics. This wiki runs no third-party scripts on reading pages.\n\nWhat we store: your account identifier from the login provider you chose, the email address that provider shares, and the public history of your edits with timestamps. Page views are not logged per user.\n\nQuestions about your data: contact the wiki administrators.\n";
const TERMS_MD: &str = "# Terms\n\nBe kind. No spam, no hate, no doxxing. Cite VODs and sources where you can. Credit artists when you post or link fan art.\n\nYour words stay yours. By publishing here you let the wiki display and archive them with your authorship attached. Administrators may edit, revert or remove pages that break these terms. Reserved usernames are granted by administrators only.\n";
const LICENSE_MD: &str = "# License\n\nThe NotAnotherWiki engine is free software under the GNU Affero General Public License v3 or later, with an exception that lets skins, templates and plugins stay closed and commercial. Nobody can turn the engine itself into a closed product.\n\nWiki text and media belong to their authors and stay exportable in an open format.\n";

pub async fn run(pool: &PgPool, skin_dir: &str) -> Result<(), AppError> {
    let templates = naw_core::templates::load_templates(skin_dir)?;
    let wiki_id = match sqlx::query!("SELECT id FROM wikis WHERE slug = 'snackers'")
        .fetch_optional(pool)
        .await?
    {
        Some(row) => row.id,
        None => {
            let id = Uuid::new_v4();
            sqlx::query!(
                "INSERT INTO wikis (id, slug, domain, name, settings) VALUES ($1, $2, $3, $4, $5)",
                id,
                "snackers",
                "snackers.vai-rice.space",
                "SnackersWIKI",
                serde_json::json!({"default": true, "home_slug": "home", "aliases": []})
            )
            .execute(pool)
            .await?;
            id
        }
    };
    for (slug, title, body) in [
        ("home", "Home", HOME_MD),
        ("about", "About", ABOUT_MD),
        ("privacy", "Privacy", PRIVACY_MD),
        ("terms", "Terms", TERMS_MD),
        ("license", "License", LICENSE_MD),
    ] {
        ensure_page(pool, &templates, wiki_id, "SnackersWIKI", slug, title, body).await?;
    }
    tracing::info!("seed: snackers wiki pages ready");
    Ok(())
}

async fn ensure_page(
    pool: &PgPool,
    env: &minijinja::Environment<'_>,
    wiki_id: Uuid,
    wiki_name: &str,
    slug: &str,
    title: &str,
    body_md: &str,
) -> Result<(), AppError> {
    if sqlx::query!(
        "SELECT id FROM pages WHERE wiki_id = $1 AND slug = $2",
        wiki_id,
        slug
    )
    .fetch_optional(pool)
    .await?
    .is_some()
    {
        return Ok(());
    }
    seed_page(pool, env, wiki_id, wiki_name, slug, title, body_md).await
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
