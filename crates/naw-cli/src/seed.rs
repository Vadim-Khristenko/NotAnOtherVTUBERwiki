//! Seeds a wiki from Markdown files in `seeds/<flavor>/`, one per page.
//!
//! Flavors: `default` for any topic, `classic` minimal, `vtuber` for a
//! streamer community, `filian` FilianWIKI copy (reuse needs permission).
//! Placeholders `{wiki_name}`, `{domain}`, `{vtuber}` and `{community}` come
//! from flags or the environment. Re-running tops up what is missing.

use sqlx::PgPool;
use uuid::Uuid;

use naw_core::error::AppError;

/// What `naw seed` needs beyond pools and directories.
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

/// Seeds or tops up one wiki.
pub async fn run(pool: &PgPool, seed_dir: &str, opts: &SeedOptions) -> Result<(), AppError> {
    let flavor_dir = format!("{seed_dir}/{}", opts.flavor);
    let wiki_id = match sqlx::query!("SELECT id FROM wikis WHERE slug = $1", opts.slug)
        .fetch_optional(pool)
        .await?
    {
        Some(row) => row.id,
        None => seed_wiki(pool, opts).await?,
    };
    sqlx::query!(
        "DELETE FROM render_cache WHERE wiki_id = $1 AND renderer_version <> $2",
        wiki_id,
        naw_markdown::RENDERER_VERSION
    )
    .execute(pool)
    .await?;
    seed_files(pool, wiki_id, opts, &flavor_dir, "main").await?;
    // Templates live in their own folder: a colon cannot be in a file name.
    let template_dir = format!("{flavor_dir}/template");
    if std::path::Path::new(&template_dir).is_dir() {
        seed_files(pool, wiki_id, opts, &template_dir, "template").await?;
    }
    // Seeded pages must be searchable.
    let indexed = naw_core::search::reindex(pool, Some(wiki_id)).await?;
    tracing::info!(
        slug = %opts.slug,
        flavor = %opts.flavor,
        indexed,
        "seed: wiki pages ready"
    );
    Ok(())
}

/// One page per `.md` file in `dir`, in `namespace`. A template names its
/// title in a `<!-- title: ... -->` line, since a heading would render.
async fn seed_files(
    pool: &PgPool,
    wiki_id: Uuid,
    opts: &SeedOptions,
    dir: &str,
    namespace: &str,
) -> Result<(), AppError> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|err| AppError::Config(format!("seed dir {dir}: {err}")))?
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
        let title = comment_title(&body_md)
            .or_else(|| first_heading(&body_md))
            .unwrap_or_else(|| slug.clone());
        let page = Page {
            namespace,
            slug: &slug,
            title: &title,
            body_md: &body_md,
        };
        ensure_page(pool, wiki_id, &opts.locale, &page).await?;
    }
    Ok(())
}

/// `<!-- title: Name -->` in the first lines of a file.
fn comment_title(body_md: &str) -> Option<String> {
    body_md
        .lines()
        .take(3)
        .find_map(|line| line.trim().strip_prefix("<!-- title:")?.strip_suffix("-->"))
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty())
}

/// One page to seed.
struct Page<'a> {
    namespace: &'a str,
    slug: &'a str,
    title: &'a str,
    body_md: &'a str,
}

fn apply_placeholders(text: &str, opts: &SeedOptions) -> String {
    text.replace("{wiki_name}", &opts.name)
        .replace("{domain}", opts.domain.as_deref().unwrap_or(""))
        .replace("{vtuber}", &opts.vtuber)
        .replace("{community}", &opts.community)
}

/// The first `{lowercase}` token left after substitution, so a typo warns
/// instead of being published.
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
    // One default wiki at most, or Host resolution depends on order.
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

async fn ensure_page(
    pool: &PgPool,
    wiki_id: Uuid,
    locale: &str,
    page: &Page<'_>,
) -> Result<(), AppError> {
    if sqlx::query!(
        "SELECT id FROM pages WHERE wiki_id = $1 AND namespace = ($3::text)::page_namespace AND slug = $2",
        wiki_id,
        page.slug,
        page.namespace
    )
    .fetch_optional(pool)
    .await?
    .is_none()
    {
        seed_content(pool, wiki_id, locale, page).await?;
    }
    ensure_cache(pool, wiki_id, page.body_md).await
}

async fn seed_content(
    pool: &PgPool,
    wiki_id: Uuid,
    locale: &str,
    page: &Page<'_>,
) -> Result<(), AppError> {
    let page_id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    let body_md = page.body_md;
    sqlx::query!(
        "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale)
         VALUES ($1, $2, ($6::text)::page_namespace, $3, $4, $5)",
        page_id,
        wiki_id,
        page.slug,
        page.title,
        locale,
        page.namespace
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

/// Renders the body into the cache when this renderer version has no row.
async fn ensure_cache(pool: &PgPool, wiki_id: Uuid, body_md: &str) -> Result<(), AppError> {
    let key = naw_markdown::content_hash(body_md);
    if sqlx::query!(
        "SELECT 1 AS present FROM render_cache WHERE wiki_id = $1 AND content_hash = $2 AND renderer_version = $3",
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
    let rendered = naw_markdown::render_body(body_md);
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
