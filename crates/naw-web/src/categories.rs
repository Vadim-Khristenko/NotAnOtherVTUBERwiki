//! Categories: `/category:витуберы` lists every page that writes
//! `[[Category:Витуберы]]`, or uses a template that does, under the
//! category's own description when it has one.
//!
//! The description is an ordinary page in the `category` namespace, with its
//! history and translations. A category has subcategories two ways: by
//! levels in its name (`[[Category:Streams/ARG]]` is ARG inside Streams, at
//! `/category:streams:arg`), and by category pages that put themselves in it.
//! A category with members and no description still has its page, and so
//! does a level nobody wrote on its own.
//!
//! Every spelling of an address lands on one: `/category:Streams/ARG`,
//! `/категория:streams:sub:arg`. `/category:streams/*` lists the pages of
//! Streams and of everything inside it, and `/category:streams/arg/filian`
//! opens the article `filian` when it is in that category.

use std::collections::{BTreeMap, HashMap};

use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;
use naw_markdown::categories::{self as cats, Membership};

use crate::pages::{self, ENGINE_VERSION};
use crate::perm::Capability;
use crate::resolve::Ctx;

/// The path prefix of a category: `/category:vtubers`.
pub(crate) const PREFIX: &str = "category:";

/// Prefixes an address may use; all of them land on [`PREFIX`].
const ALIASES: [&str; 2] = ["category:", "категория:"];

/// The key after a category prefix, as written.
pub(crate) fn strip(path: &str) -> Option<&str> {
    ALIASES.iter().find_map(|p| path.strip_prefix(p))
}

/// [`strip`], and whether the prefix was the canonical one.
pub(crate) fn strip_spelled(path: &str) -> Option<(&str, bool)> {
    strip(path).map(|rest| (rest, path.starts_with(PREFIX)))
}

/// Replaces the categories `page_id` is in.
pub(crate) async fn record(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    page_id: Uuid,
    categories: &[Membership],
) -> Result<(), sqlx::Error> {
    let keys: Vec<String> = categories.iter().map(|m| m.key.clone()).collect();
    let names: Vec<String> = categories.iter().map(|m| m.name.clone()).collect();
    let sorts: Vec<Option<String>> = categories.iter().map(|m| m.sort.clone()).collect();
    let mut tx = db.begin().await?;
    sqlx::query!("DELETE FROM page_categories WHERE page_id = $1", page_id)
        .execute(&mut *tx)
        .await?;
    // A page template is a blueprint: the categories its text names are
    // for the pages started from it.
    if !keys.is_empty() {
        sqlx::query!(
            "INSERT INTO page_categories (page_id, wiki_id, category, name, sort_key)
             SELECT $1, $2, k, n, s FROM unnest($3::text[], $4::text[], $5::text[]) AS t(k, n, s)
             WHERE EXISTS (SELECT 1 FROM pages p WHERE p.id = $1 AND p.namespace <> 'page_template')
             ON CONFLICT DO NOTHING",
            page_id,
            wiki_id,
            &keys,
            &names,
            &sorts as &[Option<String>]
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// [`record`], logging a failure: the list is rebuilt by the next save or view.
pub(crate) async fn record_or_log(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    page_id: Uuid,
    categories: &[Membership],
) {
    if let Err(err) = record(db, wiki_id, page_id, categories).await {
        tracing::warn!(error = %err, %page_id, "could not record a page's categories");
    }
}

/// Records the categories a view found when they differ from the stored
/// ones: a template edit moves pages in and out of categories with no save.
pub(crate) async fn sync(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    page_id: Uuid,
    categories: &[Membership],
) {
    let stored = sqlx::query!(
        "SELECT category, name, sort_key FROM page_categories WHERE page_id = $1",
        page_id
    )
    .fetch_all(db)
    .await;
    let Ok(stored) = stored else {
        return;
    };
    // A page template never joins its categories (see `record`).
    if stored.is_empty()
        && !categories.is_empty()
        && sqlx::query_scalar!(
            r#"SELECT (namespace = 'page_template') AS "blueprint!" FROM pages WHERE id = $1"#,
            page_id
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
    {
        return;
    }
    let mut have: Vec<(String, String, Option<String>)> = stored
        .into_iter()
        .map(|r| (r.category, r.name, r.sort_key))
        .collect();
    let mut want: Vec<(String, String, Option<String>)> = categories
        .iter()
        .map(|m| (m.key.clone(), m.name.clone(), m.sort.clone()))
        .collect();
    have.sort();
    want.sort();
    if have != want {
        record_or_log(db, wiki_id, page_id, categories).await;
    }
}

/// The address of a category in this request's language.
pub(crate) fn href(ctx: &Ctx, key: &str) -> String {
    ctx.link(&format!("/{PREFIX}{key}"))
}

/// A category's levels, each with its name and address: `Streams › ARG`.
fn trail(ctx: &Ctx, name: &str) -> Vec<minijinja::Value> {
    let mut key = String::new();
    cats::levels_in(name, cats::Shape::LOOSEST)
        .into_iter()
        .map(|level| {
            if !key.is_empty() {
                key.push(':');
            }
            key.push_str(&level.key);
            minijinja::context! { name => level.name, href => href(ctx, &key) }
        })
        .collect()
}

/// The box under a page: each category as its levels, linked one by one.
pub(crate) fn links(ctx: &Ctx, categories: &[Membership]) -> Vec<minijinja::Value> {
    categories
        .iter()
        .map(|m| {
            minijinja::context! {
                name => m.name.clone(),
                href => href(ctx, &m.key),
                trail => trail(ctx, &m.name),
            }
        })
        .collect()
}

/// `LIKE` pattern for everything inside `key`. Keys hold letters, digits,
/// `-` and `:`, none of which `LIKE` reads specially.
fn inside(key: &str) -> String {
    format!("{key}:%")
}

/// Whether anything knows `key`: a page in it or inside it, or a description.
async fn exists(db: &sqlx::PgPool, wiki_id: Uuid, key: &str) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM page_categories
             WHERE wiki_id = $1 AND (category = $2 OR category LIKE $3)
           ) OR EXISTS (
             SELECT 1 FROM pages
             WHERE wiki_id = $1 AND namespace = 'category' AND deleted_at IS NULL
               AND (slug = $2 OR slug LIKE $3)
           ) AS "known!""#,
        wiki_id,
        key,
        inside(key)
    )
    .fetch_one(db)
    .await?)
}

/// The canonical address of a category, with `?all=1` for the deep list.
fn canonical(ctx: &Ctx, key: &str, deep: bool) -> String {
    let base = href(ctx, key);
    if deep { format!("{base}?all=1") } else { base }
}

fn moved(target: &str) -> Response {
    pages::redirect_response(StatusCode::MOVED_PERMANENTLY, target)
        .unwrap_or_else(crate::errors::not_found)
}

/// Any address after a category prefix: one segment from `/{slug}`, or
/// several from the router's fallback. Lands every spelling on the
/// canonical address, then shows the category.
pub(crate) async fn route(
    state: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    rest: &str,
    canonical_prefix: bool,
    deep: bool,
) -> Result<Response, AppError> {
    let rest = rest.trim_matches('/');
    // `/streams/*` and `streams:*`: the pages of Streams and all inside it.
    let (rest, star) = match rest.strip_suffix("/*").or_else(|| rest.strip_suffix(":*")) {
        Some(head) => (head, true),
        None => (rest, rest == "*"),
    };
    let deep = deep || star;
    let shape = cats::Shape::of(&ctx.limits);
    if let Some(key) = cats::key_in(rest, shape) {
        let spelled = canonical_prefix && !star && rest == key;
        if spelled {
            return page(state, ctx, headers, &key, deep).await;
        }
        if exists(&state.db, ctx.wiki.id, &key).await? {
            return Ok(moved(&canonical(ctx, &key, deep)));
        }
    }
    // `/category:streams/arg/filian`: an article in that category.
    if let Some((head, slug)) = rest.rsplit_once('/')
        && let Some(key) = cats::key_in(head, shape)
        && let Some(target) = member_page(state, ctx, &key, slug).await?
    {
        return Ok(pages::see_other(&target));
    }
    Ok(crate::errors::not_found())
}

/// The address of the page at `slug` when it is in `key` or inside it.
async fn member_page(
    state: &AppState,
    ctx: &Ctx,
    key: &str,
    slug: &str,
) -> Result<Option<String>, AppError> {
    let row = sqlx::query!(
        r#"SELECT p.namespace::text AS "namespace!", p.slug, COALESCE(p.locale, '') AS "locale!"
           FROM page_categories pc JOIN pages p ON p.id = pc.page_id
           WHERE pc.wiki_id = $1 AND (pc.category = $2 OR pc.category LIKE $3)
             AND p.slug = $4 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
           ORDER BY (COALESCE(p.locale, '') = $5) DESC, p.namespace
           LIMIT 1"#,
        ctx.wiki.id,
        key,
        inside(key),
        slug.to_lowercase(),
        ctx.content_locale
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(row.and_then(|r| {
        let path = pages::path_of(&r.namespace, &r.slug)?;
        Some(if r.locale == ctx.content_locale {
            ctx.link(&format!("/{path}"))
        } else {
            ctx.link_for(&r.locale, &format!("/{path}"))
        })
    }))
}

/// One member as the category page lists it.
struct Member {
    namespace: String,
    slug: String,
    title: String,
    locale: String,
    sort: String,
}

/// The first letter a member is filed under.
fn letter_of(sort: &str) -> String {
    sort.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "#".to_string())
}

/// What is known about the categories inside `key`: each key with the name
/// a page wrote for its levels, and the descriptions' titles.
struct Inside {
    /// Every key inside, with a written name for each of its levels.
    names: BTreeMap<String, String>,
    /// Titles of described categories, by key.
    titles: HashMap<String, String>,
}

async fn inside_of(state: &AppState, ctx: &Ctx, key: &str) -> Result<Inside, AppError> {
    let written = sqlx::query!(
        r#"SELECT DISTINCT ON (pc.category) pc.category, pc.name
           FROM page_categories pc JOIN pages p ON p.id = pc.page_id
           WHERE pc.wiki_id = $1 AND (pc.category = $2 OR pc.category LIKE $3)
             AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
           ORDER BY pc.category, pc.name"#,
        ctx.wiki.id,
        key,
        inside(key)
    )
    .fetch_all(&state.db)
    .await?;
    let described = sqlx::query!(
        r#"SELECT DISTINCT ON (slug) slug, title FROM pages
           WHERE wiki_id = $1 AND namespace = 'category' AND deleted_at IS NULL
             AND (slug = $2 OR slug LIKE $3)
           ORDER BY slug, (COALESCE(locale, '') = $4) DESC"#,
        ctx.wiki.id,
        key,
        inside(key),
        ctx.content_locale
    )
    .fetch_all(&state.db)
    .await?;
    let mut names = BTreeMap::new();
    for row in written {
        // Each prefix of a written name names a level above it too.
        let mut prefix = String::new();
        for level in cats::levels_in(&row.name, cats::Shape::LOOSEST) {
            if !prefix.is_empty() {
                prefix.push(':');
            }
            prefix.push_str(&level.key);
            names.entry(prefix.clone()).or_insert(level.name);
        }
    }
    let mut titles = HashMap::new();
    for row in described {
        names
            .entry(row.slug.clone())
            .or_insert_with(|| row.slug.rsplit(':').next().unwrap_or("").to_string());
        titles.insert(row.slug, row.title);
    }
    Ok(Inside { names, titles })
}

/// GET /category:key, or the deep list with `all`.
async fn page(
    state: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    key: &str,
    deep: bool,
) -> Result<Response, AppError> {
    let path = format!("{PREFIX}{key}");
    // The description in the reader's language, else in the wiki's own.
    let mut description =
        pages::find_page(&state.db, ctx.wiki.id, &path, &ctx.content_locale).await?;
    if description.is_none() && ctx.content_locale != ctx.wiki.default_locale {
        description =
            pages::find_page(&state.db, ctx.wiki.id, &path, &ctx.wiki.default_locale).await?;
    }
    let found = inside_of(state, ctx, key).await?;
    let members = members(state, ctx, key, deep).await?;
    // The levels one step down, written or described.
    let depth = key.split(':').count();
    let mut children: Vec<String> = found
        .names
        .keys()
        .filter(|k| k.split(':').count() == depth + 1 && k.starts_with(&format!("{key}:")))
        .cloned()
        .collect();
    if description.is_none() && members.is_empty() && children.is_empty() {
        if !ctx.actor.can(Capability::PageCreate) {
            return Ok(crate::errors::not_found());
        }
        return pages::notice(
            ctx,
            StatusCode::NOT_FOUND,
            &ctx.t("category.empty_title"),
            &ctx.t_with(
                "category.empty_body",
                &[("code", &format!("[[Category:{key}]]"))],
            ),
            &ctx.link(&format!("/new?slug={path}")),
            &ctx.t("category.create"),
        );
    }

    let (body_html, parents) = match &description {
        Some(found) => {
            let body = pages::cached_body_full(state, ctx, &path, &found.body_md).await?;
            sync(&state.db, ctx.wiki.id, found.id, &body.categories).await;
            (Some(body.html), links(ctx, &body.categories))
        }
        None => (None, Vec::new()),
    };
    let level_name = |k: &str| -> String {
        found
            .titles
            .get(k)
            .or_else(|| found.names.get(k))
            .cloned()
            .unwrap_or_else(|| k.rsplit(':').next().unwrap_or(k).to_string())
    };
    let name = match &description {
        Some(found) => found.title.clone(),
        None => level_name(key),
    };
    // The levels above, for the crumbs; their names come from pages below.
    let ancestors: Vec<minijinja::Value> = cats::ancestors(key)
        .into_iter()
        .map(|k| {
            let title = match found.titles.get(&k) {
                Some(t) => t.clone(),
                None => found
                    .names
                    .get(&k)
                    .cloned()
                    .unwrap_or_else(|| k.rsplit(':').next().unwrap_or(&k).to_string()),
            };
            minijinja::context! { name => title, href => href(ctx, &k) }
        })
        .collect();

    let mut explicit = Vec::new();
    let mut files = Vec::new();
    let mut articles = Vec::new();
    for m in members {
        match m.namespace.as_str() {
            "category" => explicit.push(m),
            "file" => files.push(m),
            _ => articles.push(m),
        }
    }
    // A category page that put itself here is a subcategory too.
    for m in &explicit {
        if !children.contains(&m.slug) && m.slug != key {
            children.push(m.slug.clone());
        }
    }
    let counts = member_counts(&state.db, ctx.wiki.id, &children).await?;
    let explicit_titles: HashMap<&str, &str> = explicit
        .iter()
        .map(|m| (m.slug.as_str(), m.title.as_str()))
        .collect();
    let mut subcategories: Vec<(String, minijinja::Value)> = children
        .iter()
        .map(|k| {
            let title = explicit_titles
                .get(k.as_str())
                .map(|t| t.to_string())
                .unwrap_or_else(|| level_name(k));
            (
                title.to_lowercase(),
                minijinja::context! {
                    title => title,
                    href => href(ctx, k),
                    count => counts.get(k).copied().unwrap_or(0),
                },
            )
        })
        .collect();
    subcategories.sort_by(|a, b| a.0.cmp(&b.0));
    let subcategories: Vec<minijinja::Value> = subcategories.into_iter().map(|(_, v)| v).collect();
    let articles: Vec<minijinja::Value> = articles
        .iter()
        .map(|m| {
            let path = pages::path_of(&m.namespace, &m.slug);
            let href = match &path {
                Some(path) if m.locale == ctx.content_locale => ctx.link(&format!("/{path}")),
                Some(path) => ctx.link_for(&m.locale, &format!("/{path}")),
                None => format!("/user/{}", m.slug),
            };
            minijinja::context! {
                title => m.title,
                href => href,
                letter => letter_of(&m.sort),
                // a page with no version in the reader's language, in its own
                locale => (m.locale != ctx.content_locale).then(|| m.locale.clone()),
                kind => (m.namespace != "main").then(|| ctx.t(&format!("category.kind_{}", m.namespace))),
            }
        })
        .collect();
    let files = file_tiles(state, ctx, &files).await?;

    let may_edit = description
        .as_ref()
        .is_some_and(|d| ctx.actor.can_edit_page(d.protection));
    let template = ctx
        .skin
        .env
        .get_template("category.html")
        .map_err(pages::template_error)?;
    let heading = ctx.t_with("category.heading", &[("name", &name)]);
    // The line that adds a page here spells each level as pages write it, not
    // as a description titles it: a Russian title would name another category.
    let written: Vec<String> = cats::ancestors(key)
        .into_iter()
        .chain(std::iter::once(key.to_string()))
        .map(|k| {
            found
                .names
                .get(&k)
                .cloned()
                .unwrap_or_else(|| k.rsplit(':').next().unwrap_or(&k).to_string())
        })
        .collect();
    let has_inside = !children.is_empty();
    // Search engines keep the category itself, not its deep list.
    let seo =
        (!deep).then(|| {
            crate::seo::head(
                ctx,
                crate::seo::Card {
                    title: &heading,
                    description: body_html
                        .as_deref()
                        .and_then(crate::seo::description)
                        .or_else(|| {
                            Some(ctx.t_with(
                                "seo.category",
                                &[("name", &name), ("wiki", &ctx.wiki.name)],
                            ))
                        }),
                    href: canonical(ctx, key, false),
                    image: body_html.as_deref().and_then(crate::seo::first_image),
                    article: false,
                    languages: Vec::new(),
                },
            )
        });
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => heading.clone(),
                heading => heading,
                version => ENGINE_VERSION,
                slug => path.clone(),
                name => name,
                ancestors => ancestors,
                code => format!("[[Category:{}]]", written.join("/")),
                child_code => format!("[[Category:{}/…]]", written.join("/")),
                description => body_html,
                may_edit => may_edit,
                may_create => description.is_none() && ctx.actor.can(Capability::PageCreate),
                create_href => ctx.link(&format!("/new?slug={path}")),
                subcategories => subcategories,
                pages => articles,
                files => files,
                categories => parents,
                members_max => ctx.limits.category_pages_shown,
                deep => deep,
                has_inside => has_inside,
                direct_href => canonical(ctx, key, false),
                deep_href => canonical(ctx, key, true),
                seo => seo,
            }
        })
        .map_err(pages::template_error)?;
    Ok(pages::html_response(html, headers))
}

/// The pages in a category, or with `deep` in it and everything inside it,
/// once each: the version in the reader's language when there is one,
/// sorted as the category files them.
async fn members(
    state: &AppState,
    ctx: &Ctx,
    key: &str,
    deep: bool,
) -> Result<Vec<Member>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (p.namespace, p.slug)
                  p.namespace::text AS "namespace!", p.slug, p.title,
                  COALESCE(p.locale, '') AS "locale!", pc.sort_key
           FROM page_categories pc JOIN pages p ON p.id = pc.page_id
           WHERE pc.wiki_id = $1 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND (pc.category = $2 OR ($6 AND pc.category LIKE $7))
           ORDER BY p.namespace, p.slug, (COALESCE(p.locale, '') = $3) DESC,
                    (COALESCE(p.locale, '') = $4) DESC
           LIMIT $5"#,
        ctx.wiki.id,
        key,
        ctx.content_locale,
        ctx.wiki.default_locale,
        ctx.limits.category_pages_shown,
        deep,
        inside(key)
    )
    .fetch_all(&state.db)
    .await?;
    let mut members: Vec<Member> = rows
        .into_iter()
        .map(|row| Member {
            sort: row.sort_key.unwrap_or_else(|| row.title.clone()),
            namespace: row.namespace,
            slug: row.slug,
            title: row.title,
            locale: row.locale,
        })
        .collect();
    members.sort_by_cached_key(|m| (m.sort.to_lowercase(), m.slug.clone()));
    Ok(members)
}

/// How many pages each category has with everything inside it, counting a
/// page once in all languages.
pub(crate) async fn member_counts(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    keys: &[String],
) -> Result<HashMap<String, i64>, AppError> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query!(
        r#"SELECT c.k AS "key!", count(DISTINCT p.namespace::text || ':' || p.slug) AS "n!"
           FROM unnest($2::text[]) AS c(k)
           JOIN page_categories pc ON pc.wiki_id = $1
             AND (pc.category = c.k OR pc.category LIKE c.k || ':%')
           JOIN pages p ON p.id = pc.page_id AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
           GROUP BY c.k"#,
        wiki_id,
        keys
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|r| (r.key, r.n)).collect())
}

/// Files in a category, as tiles; a hidden file is left out.
async fn file_tiles(
    state: &AppState,
    ctx: &Ctx,
    members: &[Member],
) -> Result<Vec<minijinja::Value>, AppError> {
    if members.is_empty() {
        return Ok(Vec::new());
    }
    let names: Vec<String> = members.iter().map(|m| m.slug.clone()).collect();
    let rows = sqlx::query!(
        "SELECT name, kind, storage_key, width, height FROM media
         WHERE wiki_id = $1 AND name = ANY($2) AND hidden_at IS NULL",
        ctx.wiki.id,
        &names
    )
    .fetch_all(&state.db)
    .await?;
    let by_name: HashMap<String, _> = rows.into_iter().map(|r| (r.name.clone(), r)).collect();
    Ok(members
        .iter()
        .filter_map(|m| by_name.get(&m.slug))
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
        .collect())
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::PgPool;

    async fn wiki(db: &PgPool, slug: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO wikis (id, slug, name) VALUES ($1, $2, $2)")
            .bind(id)
            .bind(slug)
            .execute(db)
            .await
            .expect("wiki");
        id
    }

    async fn page(db: &PgPool, wiki_id: Uuid, slug: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale)
             VALUES ($1, $2, 'main', $3, $3, 'en')",
        )
        .bind(id)
        .bind(wiki_id)
        .bind(slug)
        .execute(db)
        .await
        .expect("page");
        // A page counts once it has accepted text.
        let rev = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO revisions (id, page_id, body_md, content_hash) VALUES ($1, $2, 'x', '\\x00')",
        )
        .bind(rev)
        .bind(id)
        .execute(db)
        .await
        .expect("revision");
        sqlx::query("UPDATE pages SET current_revision_id = $2 WHERE id = $1")
            .bind(id)
            .bind(rev)
            .execute(db)
            .await
            .expect("current");
        id
    }

    fn cat(name: &str) -> Membership {
        Membership {
            key: cats::key(name).expect("key"),
            name: name.to_string(),
            sort: None,
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_category_counts_only_its_own_wiki(db: PgPool) {
        let here = wiki(&db, "here").await;
        let there = wiki(&db, "there").await;
        let a = page(&db, here, "a").await;
        let b = page(&db, here, "b").await;
        let c = page(&db, there, "c").await;
        record(&db, here, a, &[cat("VTubers"), cat("Витуберы")])
            .await
            .expect("a");
        record(&db, here, b, &[cat("vtubers")]).await.expect("b");
        record(&db, there, c, &[cat("VTubers")]).await.expect("c");

        let keys = vec!["vtubers".to_string(), "витуберы".to_string()];
        let here_counts = member_counts(&db, here, &keys).await.expect("counts");
        assert_eq!(here_counts.get("vtubers"), Some(&2));
        assert_eq!(here_counts.get("витуберы"), Some(&1));
        let there_counts = member_counts(&db, there, &keys).await.expect("counts");
        assert_eq!(
            there_counts.get("vtubers"),
            Some(&1),
            "the other wiki has its own"
        );
        assert_eq!(there_counts.get("витуберы"), None);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_level_counts_the_pages_inside_it(db: PgPool) {
        let here = wiki(&db, "here").await;
        let there = wiki(&db, "there").await;
        let a = page(&db, here, "a").await;
        let b = page(&db, here, "b").await;
        let c = page(&db, here, "c").await;
        let d = page(&db, there, "d").await;
        record(&db, here, a, &[cat("Streams/ARG")])
            .await
            .expect("a");
        record(&db, here, b, &[cat("Streams:ARG/2024"), cat("Streams")])
            .await
            .expect("b");
        record(&db, here, c, &[cat("Streamsy")]).await.expect("c");
        record(&db, there, d, &[cat("Streams/ARG")])
            .await
            .expect("d");

        let keys = vec!["streams".to_string(), "streams:arg".to_string()];
        let counts = member_counts(&db, here, &keys).await.expect("counts");
        assert_eq!(
            counts.get("streams"),
            Some(&2),
            "a and b, not the look-alike Streamsy"
        );
        assert_eq!(counts.get("streams:arg"), Some(&2));
        assert!(exists(&db, here, "streams").await.expect("exists"));
        assert!(exists(&db, here, "streams:arg:2024").await.expect("exists"));
        assert!(!exists(&db, here, "streams:lore").await.expect("exists"));
        let there_counts = member_counts(&db, there, &keys).await.expect("counts");
        assert_eq!(there_counts.get("streams"), Some(&1));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_page_leaves_a_category_when_its_text_does(db: PgPool) {
        let here = wiki(&db, "here").await;
        let a = page(&db, here, "a").await;
        record(&db, here, a, &[cat("VTubers"), cat("Lore")])
            .await
            .expect("first");
        sync(&db, here, a, &[cat("Lore")]).await;
        let keys: Vec<String> =
            sqlx::query_scalar("SELECT category FROM page_categories WHERE page_id = $1")
                .bind(a)
                .fetch_all(&db)
                .await
                .expect("keys");
        assert_eq!(keys, ["lore"]);
        sync(&db, here, a, &[]).await;
        let left: i64 =
            sqlx::query_scalar("SELECT count(*) FROM page_categories WHERE page_id = $1")
                .bind(a)
                .fetch_one(&db)
                .await
                .expect("count");
        assert_eq!(left, 0);
    }
}
