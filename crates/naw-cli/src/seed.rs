//! Seeds a wiki from Markdown files. Content lives in `seeds/<flavor>/` as
//! one file per page, so humans edit files instead of code. Flavors:
//! `default` fits any topic, `classic` is the bare minimum, `vtuber`
//! frames a streamer community, `filian` is baked FilianWIKI copy that
//! needs written permission to reuse as is.
//!
//! Placeholders in the files come from the command line or the environment:
//! `{wiki_name}`, `{domain}`, `{vtuber}`, `{community}`. Re-running tops up
//! whatever is missing, so a flavor change heals itself on the next seed.

use sqlx::PgPool;
use uuid::Uuid;

use naw_core::error::AppError;

/// Engine version baked into seeded footers. Tracks the workspace release.
const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Everything `naw seed` needs beyond pools and directories.
pub struct SeedOptions {
    pub flavor: String,
    pub slug: String,
    pub name: String,
    pub domain: Option<String>,
    pub locale: String,
    pub vtuber: String,
    pub community: String,
    pub aliases: Vec<String>,
}

pub async fn run(
    pool: &PgPool,
    skin_dir: &str,
    seed_dir: &str,
    opts: &SeedOptions,
) -> Result<(), AppError> {
    let templates = naw_core::templates::load_templates(skin_dir)?;
    let flavor_dir = format!("{seed_dir}/{}", opts.flavor);
    let wiki_id = match sqlx::query!("SELECT id FROM wikis WHERE slug = $1", opts.slug)
        .fetch_optional(pool)
        .await?
    {
        Some(row) => row.id,
        None => seed_wiki(pool, opts).await?,
    };
    // Chrome renders under the wiki's stored name, never under the CLI
    // flag default (which is the slug). Seeding an existing wiki with a bare
    // --slug once poisoned every cache row with that slug as the brand.
    let wiki_name: String = sqlx::query!("SELECT name FROM wikis WHERE id = $1", wiki_id)
        .fetch_one(pool)
        .await?
        .name;
    sqlx::query!(
        "DELETE FROM render_cache WHERE wiki_id = $1 AND renderer_version <> $2",
        wiki_id,
        naw_markdown::RENDERER_VERSION
    )
    .execute(pool)
    .await?;
    let mut entries: Vec<_> = std::fs::read_dir(&flavor_dir)
        .map_err(|err| AppError::Config(format!("seed dir {flavor_dir}: {err}")))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "md")
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let slug = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        if slug.is_empty() {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|err| AppError::Config(format!("seed file {}: {err}", path.display())))?;
        let body_md = apply_placeholders(&raw, opts);
        if let Some(token) = leftover_placeholder(&body_md) {
            tracing::warn!(
                file = %path.display(),
                token,
                "seed file has an unfilled placeholder"
            );
        }
        let title = first_heading(&body_md).unwrap_or_else(|| slug.clone());
        ensure_page(
            pool,
            &templates,
            wiki_id,
            &wiki_name,
            &opts.locale,
            &slug,
            &title,
            &body_md,
            skin_dir,
        )
        .await?;
    }
    tracing::info!(slug = %opts.slug, flavor = %opts.flavor, "seed: wiki pages ready");
    Ok(())
}

fn apply_placeholders(text: &str, opts: &SeedOptions) -> String {
    text.replace("{wiki_name}", &opts.name)
        .replace("{domain}", opts.domain.as_deref().unwrap_or(""))
        .replace("{vtuber}", &opts.vtuber)
        .replace("{community}", &opts.community)
}

/// Finds the first `{lowercase}` token left after substitution, so typos
/// in placeholder names surface as a warning instead of published text.
fn leftover_placeholder(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let end = after.find('}')?;
        let token = &after[..end];
        if !token.is_empty() && token.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return Some(token.to_string());
        }
        rest = &after[end + 1..];
    }
    None
}

fn first_heading(body_md: &str) -> Option<String> {
    body_md
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty())
}

async fn seed_wiki(pool: &PgPool, opts: &SeedOptions) -> Result<Uuid, AppError> {
    let id = Uuid::new_v4();
    let aliases =
        serde_json::to_value(&opts.aliases).map_err(|err| AppError::Config(err.to_string()))?;
    // Exactly one default wiki may exist: a second default makes Host
    // resolution order dependent. A seed only becomes the default when none
    // is flagged yet.
    let has_default = sqlx::query!("SELECT id FROM wikis WHERE settings->>'default' = 'true'")
        .fetch_optional(pool)
        .await?
        .is_some();
    sqlx::query!(
        "INSERT INTO wikis (id, slug, domain, name, settings) VALUES ($1, $2, $3, $4, $5)",
        id,
        opts.slug,
        opts.domain,
        opts.name,
        serde_json::json!({"default": !has_default, "home_slug": "home", "aliases": aliases})
    )
    .execute(pool)
    .await?;
    Ok(id)
}

// Eight coherent page coordinates; splitting them into a struct would hide
// what ensure/seed/cache each need. Allowed until page writes grow further.
#[allow(clippy::too_many_arguments)]
async fn ensure_page(
    pool: &PgPool,
    env: &minijinja::Environment<'_>,
    wiki_id: Uuid,
    wiki_name: &str,
    locale: &str,
    slug: &str,
    title: &str,
    body_md: &str,
    skin_dir: &str,
) -> Result<(), AppError> {
    if sqlx::query!(
        "SELECT id FROM pages WHERE wiki_id = $1 AND slug = $2",
        wiki_id,
        slug
    )
    .fetch_optional(pool)
    .await?
    .is_none()
    {
        seed_content(pool, wiki_id, locale, slug, title, body_md).await?;
    }
    ensure_cache(
        pool, env, wiki_id, wiki_name, locale, title, body_md, skin_dir,
    )
    .await
}

async fn seed_content(
    pool: &PgPool,
    wiki_id: Uuid,
    locale: &str,
    slug: &str,
    title: &str,
    body_md: &str,
) -> Result<(), AppError> {
    let page_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale) VALUES ($1, $2, 'main', $3, $4, $5)",
        page_id,
        wiki_id,
        slug,
        title,
        locale
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        "INSERT INTO revisions (id, page_id, body_md, content_hash, summary) VALUES ($1, $2, $3, $4, NULL)",
        revision_id,
        page_id,
        body_md,
        naw_markdown::content_hash(body_md)
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
    Ok(())
}

/// Renders the page into the cache when the current renderer version has
/// no row yet. This heals stale skins: bump the version, reseed, done.
// Nine coherent page coordinates, same reason as ensure_page above.
#[allow(clippy::too_many_arguments)]
async fn ensure_cache(
    pool: &PgPool,
    env: &minijinja::Environment<'_>,
    wiki_id: Uuid,
    wiki_name: &str,
    locale: &str,
    title: &str,
    body_md: &str,
    skin_dir: &str,
) -> Result<(), AppError> {
    let key = naw_markdown::page_hash(title, locale, body_md, "", skin_dir);
    if sqlx::query!(
        "SELECT html FROM render_cache WHERE wiki_id = $1 AND content_hash = $2 AND renderer_version = $3",
        wiki_id,
        key,
        naw_markdown::RENDERER_VERSION
    )
    .fetch_optional(pool)
    .await?
    .is_some()
    {
        return Ok(());
    }
    let rendered = naw_markdown::render_page(
        env,
        &naw_markdown::PageInput {
            title,
            body_md,
            wiki_name,
            lang: locale,
            version: ENGINE_VERSION,
            served_from_cache: false,
            summary: "",
            skin: skin_dir,
        },
    )?;
    sqlx::query!(
        "INSERT INTO render_cache (wiki_id, content_hash, renderer_version, html) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        wiki_id,
        rendered.content_hash,
        naw_markdown::RENDERER_VERSION,
        rendered.html
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_opts() -> SeedOptions {
        SeedOptions {
            flavor: "default".to_string(),
            slug: "wiki".to_string(),
            name: "W".to_string(),
            domain: Some("d.test".to_string()),
            locale: "en".to_string(),
            vtuber: "V".to_string(),
            community: "fans".to_string(),
            aliases: Vec::new(),
        }
    }

    #[test]
    fn placeholders_fill_and_title_parses() {
        let opts = test_opts();
        let body = apply_placeholders(
            "# Welcome to {wiki_name} on {domain} with {vtuber} and {community}\n",
            &opts,
        );
        assert_eq!(body, "# Welcome to W on d.test with V and fans\n");
        assert_eq!(
            first_heading(&body).as_deref(),
            Some("Welcome to W on d.test with V and fans")
        );
        assert_eq!(first_heading("no heading here\n"), None);
        assert_eq!(leftover_placeholder("clean {Wiki} and {a b} text\n"), None);
        assert_eq!(
            leftover_placeholder("typo {wiki_nmae} here\n").as_deref(),
            Some("wiki_nmae")
        );
    }
}
