//! Special pages, as `/system:name`: lists and figures the wiki keeps about
//! itself rather than pages anyone writes. `/system` lists them all, in
//! groups: finding your way, what is going on, and what needs work.

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::pages::{self, ENGINE_VERSION};
use crate::perm::Capability;
use crate::resolve::Ctx;

const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

/// The special pages by group, in the order `/system` lists them, with their icon.
const GROUPS: [(&str, &[(&str, &str)]); 3] = [
    (
        "browse",
        &[
            ("all-pages", "list"),
            ("categories", "list-details"),
            ("new-pages", "sparkles"),
            ("files", "photo"),
            ("templates", "template"),
            ("random", "arrows-shuffle"),
        ],
    ),
    (
        "activity",
        &[
            ("recent-changes", "activity"),
            ("active-users", "users-group"),
            ("statistics", "chart-bar"),
        ],
    ),
    (
        "maintenance",
        &[
            ("uncategorized", "help-circle"),
            ("wanted-categories", "file-plus"),
            ("untranslated", "language"),
            ("short-pages", "file-text"),
            ("long-pages", "book"),
            ("stale-pages", "calendar"),
            ("unused-files", "archive"),
        ],
    ),
];

/// Rows on one screen of a list.
const RECENT_MAX: i64 = 100;
const ALL_PAGES_STEP: i64 = 300;
const FILES_MAX: i64 = 120;
const LIST_MAX: i64 = 200;

/// An edit this many bytes or more either way is shown in bold.
const BIG_EDIT_BYTES: i32 = 500;

/// The namespaces All pages can list, as the database spells them.
const LISTED_NAMESPACES: [&str; 4] = ["main", "category", "template", "page_template"];

/// What a special page may be asked for in its query.
#[derive(Debug, Default)]
pub(crate) struct Query {
    /// All pages: start at the first title from here on.
    pub from: Option<String>,
    /// All pages: which namespace.
    pub ns: Option<String>,
}

/// GET /system and /system:name. `None` for a name that is not one.
pub(crate) async fn page(
    state: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    name: &str,
    query: &Query,
) -> Result<Option<Response>, AppError> {
    let (view, extra) = match name {
        "" => ("index", index(ctx)),
        "recent-changes" => ("recent-changes", recent_changes(state, ctx).await?),
        "all-pages" => ("all-pages", all_pages(state, ctx, query).await?),
        "files" => ("files", files(state, ctx).await?),
        "statistics" => ("statistics", statistics(state, ctx).await?),
        "random" => return random(state, ctx).await.map(Some),
        "categories" => ("categories", categories(state, ctx, false).await?),
        "wanted-categories" => ("wanted-categories", categories(state, ctx, true).await?),
        "uncategorized" => ("uncategorized", uncategorized(state, ctx).await?),
        "new-pages" => ("new-pages", new_pages(state, ctx).await?),
        "untranslated" => ("untranslated", untranslated(state, ctx).await?),
        "short-pages" => ("short-pages", by_size(state, ctx, true).await?),
        "long-pages" => ("long-pages", by_size(state, ctx, false).await?),
        "stale-pages" => ("stale-pages", stale_pages(state, ctx).await?),
        "unused-files" => ("unused-files", unused_files(state, ctx).await?),
        "templates" => ("templates", templates(state, ctx).await?),
        "active-users" => ("active-users", active_users(state, ctx).await?),
        _ => return Ok(None),
    };
    let key = view.replace('-', "_");
    let title = ctx.t(&format!("system.{key}"));
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
                about => ctx.t(&format!("system.{key}_about")),
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
    let groups: Vec<minijinja::Value> = GROUPS
        .iter()
        .map(|(group, entries)| {
            let entries: Vec<minijinja::Value> = entries
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
            minijinja::context! {
                title => ctx.t(&format!("system.group_{group}")),
                entries => entries,
            }
        })
        .collect();
    minijinja::context! { groups => groups }
}

/// Where a page of any namespace lives.
fn href_of(ctx: &Ctx, namespace: &str, slug: &str) -> String {
    match pages::path_of(namespace, slug) {
        Some(path) => ctx.link(&format!("/{path}")),
        None => format!("/user/{slug}"),
    }
}

/// One row of a plain list: a title with a link, a line about it, and at
/// most one tag and one action.
fn row(title: String, href: String, meta: String) -> minijinja::Value {
    minijinja::context! { title => title, href => href, meta => meta }
}

fn row_with(
    title: String,
    href: String,
    meta: String,
    tag: Option<String>,
    action: Option<(String, String)>,
) -> minijinja::Value {
    minijinja::context! {
        title => title,
        href => href,
        meta => meta,
        tag => tag,
        action_href => action.as_ref().map(|a| a.0.clone()),
        action_label => action.map(|a| a.1),
    }
}

fn list(rows: Vec<minijinja::Value>) -> minijinja::Value {
    let shown = rows.len();
    minijinja::context! { rows => rows, list_max => LIST_MAX, at_limit => shown as i64 >= LIST_MAX }
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
            let path = pages::path_of(&row.namespace, &row.slug);
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

/// The letter a title is filed under.
fn letter_of(title: &str) -> String {
    title
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "#".to_string())
}

async fn all_pages(
    state: &AppState,
    ctx: &Ctx,
    query: &Query,
) -> Result<minijinja::Value, AppError> {
    let ns = query
        .ns
        .as_deref()
        .filter(|ns| LISTED_NAMESPACES.contains(ns))
        .unwrap_or("main");
    let from: String = query
        .from
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    // Templates and categories live in every language at once; articles in one.
    let one_language = ns == "main";
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = ($4::text)::page_namespace
             AND p.deleted_at IS NULL
             AND (NOT $5 OR COALESCE(p.locale, '') = $2)
             AND lower(p.title) >= lower($6)
           ORDER BY lower(p.title), p.slug
           LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        ALL_PAGES_STEP + 1,
        ns,
        one_language,
        from
    )
    .fetch_all(&state.db)
    .await?;
    let counts = sqlx::query!(
        r#"SELECT p.namespace::text AS "namespace!", count(DISTINCT p.slug) AS "n!"
           FROM pages p
           WHERE p.wiki_id = $1 AND p.deleted_at IS NULL
             AND p.namespace IN ('main', 'category', 'template', 'page_template')
             AND (p.namespace <> 'main' OR COALESCE(p.locale, '') = $2)
           GROUP BY p.namespace"#,
        ctx.wiki.id,
        ctx.content_locale
    )
    .fetch_all(&state.db)
    .await?;
    let letters = sqlx::query_scalar!(
        r#"SELECT DISTINCT upper(left(p.title, 1)) AS "letter!" FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = ($3::text)::page_namespace
             AND p.deleted_at IS NULL AND (NOT $4 OR COALESCE(p.locale, '') = $2)
           ORDER BY 1"#,
        ctx.wiki.id,
        ctx.content_locale,
        ns,
        one_language
    )
    .fetch_all(&state.db)
    .await?;

    let here = |from: &str| {
        let mut href = ctx.link("/system:all-pages");
        let mut sep = '?';
        if ns != "main" {
            href.push_str(&format!("{sep}ns={ns}"));
            sep = '&';
        }
        if !from.is_empty() {
            href.push_str(&format!("{sep}from={}", pages::urlencode(from)));
        }
        href
    };
    let has_more = rows.len() as i64 > ALL_PAGES_STEP;
    let next = has_more.then(|| here(&rows[ALL_PAGES_STEP as usize].title));
    // A template has one row per language; the list shows it once.
    let mut seen = std::collections::HashSet::new();
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .take(ALL_PAGES_STEP as usize)
        .filter(|row| seen.insert(row.slug.clone()))
        .map(|row| {
            minijinja::context! {
                letter => letter_of(&row.title),
                href => href_of(ctx, ns, &row.slug),
                title => row.title,
            }
        })
        .collect();
    let tabs: Vec<minijinja::Value> = LISTED_NAMESPACES
        .iter()
        .map(|id| {
            let n = counts
                .iter()
                .find(|c| c.namespace == *id)
                .map_or(0, |c| c.n);
            let mut href = ctx.link("/system:all-pages");
            if *id != "main" {
                href.push_str(&format!("?ns={id}"));
            }
            minijinja::context! {
                label => ctx.t(&format!("system.ns_{id}")),
                count => n,
                href => href,
                current => *id == ns,
            }
        })
        .collect();
    let letters: Vec<minijinja::Value> = letters
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| minijinja::context! { href => here(&l), letter => l })
        .collect();
    let total = counts.iter().find(|c| c.namespace == ns).map_or(0, |c| c.n);
    Ok(minijinja::context! {
        pages => items,
        total => total,
        tabs => tabs,
        letters => letters,
        ns => ns,
        from => from,
        next => next,
        first => (!query.from.as_deref().unwrap_or("").is_empty()).then(|| here("")),
        action => ctx.link("/system:all-pages"),
    })
}

async fn files(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        "SELECT storage_key, name, kind, width, height FROM media
         WHERE wiki_id = $1 AND hidden_at IS NULL ORDER BY created_at DESC LIMIT $2",
        ctx.wiki.id,
        FILES_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            file_tile(
                ctx,
                &row.storage_key,
                &row.name,
                &row.kind,
                row.width,
                row.height,
            )
        })
        .collect();
    Ok(minijinja::context! { files => items })
}

fn file_tile(
    ctx: &Ctx,
    storage_key: &str,
    name: &str,
    kind: &str,
    width: Option<i32>,
    height: Option<i32>,
) -> minijinja::Value {
    minijinja::context! {
        url => crate::media::url_for_key(storage_key),
        page => ctx.link(&format!("/{}:{}", crate::files::prefix_of(kind), name)),
        name => name,
        kind => kind,
        width => width,
        height => height,
    }
}

async fn statistics(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let row = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL) AS "articles!",
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'template' AND deleted_at IS NULL) AS "templates!",
             (SELECT count(DISTINCT category) FROM page_categories WHERE wiki_id = $1) AS "categories!",
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
        ("categories", row.categories.to_string()),
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

/// Every category with its size, or only those nobody described yet.
async fn categories(
    state: &AppState,
    ctx: &Ctx,
    wanted_only: bool,
) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"WITH used AS (
             SELECT pc.category, min(pc.name) AS name,
                    count(DISTINCT p.namespace::text || ':' || p.slug) AS n
             FROM page_categories pc JOIN pages p ON p.id = pc.page_id
             WHERE pc.wiki_id = $1 AND p.deleted_at IS NULL
             GROUP BY pc.category
           ), described AS (
             SELECT DISTINCT ON (slug) slug, title FROM pages
             WHERE wiki_id = $1 AND namespace = 'category' AND deleted_at IS NULL
             ORDER BY slug, (COALESCE(locale, '') = $2) DESC
           )
           SELECT COALESCE(u.category, d.slug) AS "key!", COALESCE(d.title, u.name) AS "name!",
                  COALESCE(u.n, 0) AS "n!", (d.slug IS NOT NULL) AS "described!"
           FROM used u FULL JOIN described d ON d.slug = u.category
           WHERE NOT $3 OR d.slug IS NULL
           ORDER BY lower(COALESCE(d.title, u.name))
           LIMIT 2000"#,
        ctx.wiki.id,
        ctx.content_locale,
        wanted_only
    )
    .fetch_all(&state.db)
    .await?;
    let may_create = ctx.actor.can(Capability::PageCreate);
    // A level written only inside a longer name (Streams in Streams/ARG) is
    // a category too, with no pages of its own.
    struct Entry {
        name: String,
        n: i64,
        described: bool,
    }
    let mut entries: std::collections::BTreeMap<String, Entry> = std::collections::BTreeMap::new();
    for r in rows {
        let levels = naw_markdown::categories::levels(&r.name);
        let mut prefix = String::new();
        let mut shown = Vec::new();
        for level in &levels {
            if !prefix.is_empty() {
                prefix.push(':');
            }
            prefix.push_str(&level.key);
            shown.push(level.name.clone());
            if prefix != r.key && !wanted_only {
                entries.entry(prefix.clone()).or_insert(Entry {
                    name: shown.join(" › "),
                    n: 0,
                    described: false,
                });
            }
        }
        let name = if r.described || shown.is_empty() {
            r.name
        } else {
            shown.join(" › ")
        };
        entries.insert(
            r.key,
            Entry {
                name,
                n: r.n,
                described: r.described,
            },
        );
    }
    let rows = entries
        .into_iter()
        .map(|(key, e)| {
            let path = format!("{}{key}", crate::categories::PREFIX);
            let meta = if e.n == 0 && !e.described {
                ctx.t("system.only_subcategories")
            } else {
                ctx.tn_with("category.members", e.n, &[])
            };
            row_with(
                e.name,
                crate::categories::href(ctx, &key),
                meta,
                (!e.described && !wanted_only).then(|| ctx.t("system.no_description")),
                (!e.described && may_create).then(|| {
                    (
                        ctx.link(&format!("/new?slug={}", pages::urlencode(&path))),
                        ctx.t("category.create"),
                    )
                }),
            )
        })
        .collect();
    Ok(list(rows))
}

async fn uncategorized(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, p.updated_at FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
             AND COALESCE(p.locale, '') = $2
             AND NOT EXISTS (SELECT 1 FROM page_categories pc WHERE pc.page_id = p.id)
           ORDER BY lower(p.title) LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        LIST_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let rows = rows
        .into_iter()
        .map(|r| {
            row(
                r.title,
                href_of(ctx, "main", &r.slug),
                ctx.t_with("system.edited_on", &[("when", &ctx.day(r.updated_at))]),
            )
        })
        .collect();
    Ok(list(rows))
}

async fn new_pages(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, p.created_at,
                  (SELECT u.username FROM revisions r JOIN users u ON u.id = r.author_id
                   WHERE r.page_id = p.id ORDER BY r.created_at, r.id LIMIT 1) AS "author?"
           FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
             AND COALESCE(p.locale, '') = $2
           ORDER BY p.created_at DESC LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        LIST_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let rows = rows
        .into_iter()
        .map(|r| {
            let when = ctx.day(r.created_at);
            let meta = match &r.author {
                Some(who) => ctx.t_with("system.created_by", &[("when", &when), ("who", who)]),
                None => ctx.t_with("system.created_on", &[("when", &when)]),
            };
            row(r.title, href_of(ctx, "main", &r.slug), meta)
        })
        .collect();
    Ok(list(rows))
}

/// Articles with no version in the reader's language, from the one in the
/// wiki's language when there is one.
async fn untranslated(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (p.slug) p.slug, p.title, COALESCE(p.locale, '') AS "locale!"
           FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM pages q
               WHERE q.wiki_id = p.wiki_id AND q.namespace = 'main' AND q.slug = p.slug
                 AND COALESCE(q.locale, '') = $2 AND q.deleted_at IS NULL)
           ORDER BY p.slug, (COALESCE(p.locale, '') = $3) DESC
           LIMIT $4"#,
        ctx.wiki.id,
        ctx.content_locale,
        ctx.wiki.default_locale,
        LIST_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let may_translate = ctx.actor.can(Capability::PageCreate);
    let mut rows: Vec<_> = rows
        .into_iter()
        .map(|r| {
            let action = may_translate.then(|| {
                (
                    format!(
                        "{}?from={}",
                        ctx.link(&format!("/{}/translate", r.slug)),
                        r.locale
                    ),
                    ctx.t("system.translate"),
                )
            });
            (
                r.title.to_lowercase(),
                row_with(
                    r.title,
                    ctx.link_for(&r.locale, &format!("/{}", r.slug)),
                    ctx.t_with(
                        "system.only_in",
                        &[("language", &crate::translate::native_name(ctx, &r.locale))],
                    ),
                    Some(r.locale),
                    action,
                ),
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(list(rows.into_iter().map(|(_, r)| r).collect()))
}

/// Size as a reader counts it.
fn size(ctx: &Ctx, bytes: i32) -> String {
    crate::files::human_size(ctx, i64::from(bytes))
}

/// The shortest articles, stubs to grow, or the longest, to split.
async fn by_size(
    state: &AppState,
    ctx: &Ctx,
    shortest: bool,
) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, r.bytes AS "bytes!" FROM pages p
           JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
             AND COALESCE(p.locale, '') = $2
           ORDER BY CASE WHEN $3 THEN r.bytes ELSE -r.bytes END, lower(p.title)
           LIMIT $4"#,
        ctx.wiki.id,
        ctx.content_locale,
        shortest,
        LIST_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let rows = rows
        .into_iter()
        .map(|r| row(r.title, href_of(ctx, "main", &r.slug), size(ctx, r.bytes)))
        .collect();
    Ok(list(rows))
}

async fn stale_pages(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, p.updated_at FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
             AND COALESCE(p.locale, '') = $2
           ORDER BY p.updated_at, lower(p.title) LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        LIST_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let rows = rows
        .into_iter()
        .map(|r| {
            row(
                r.title,
                href_of(ctx, "main", &r.slug),
                ctx.t_with("system.edited_on", &[("when", &ctx.day(r.updated_at))]),
            )
        })
        .collect();
    Ok(list(rows))
}

/// Files no page shows, in any of their versions, oldest first.
async fn unused_files(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        "SELECT m.storage_key, m.name, m.kind, m.width, m.height FROM media m
         WHERE m.wiki_id = $1 AND m.hidden_at IS NULL
           AND NOT EXISTS (
             SELECT 1 FROM media_versions v JOIN file_uses f ON f.storage_key = v.storage_key
             WHERE v.media_id = m.id AND f.wiki_id = $1)
         ORDER BY m.created_at LIMIT $2",
        ctx.wiki.id,
        FILES_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            file_tile(
                ctx,
                &row.storage_key,
                &row.name,
                &row.kind,
                row.width,
                row.height,
            )
        })
        .collect();
    Ok(minijinja::context! { files => items })
}

/// Templates by how many pages use them, the unused ones last.
async fn templates(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (p.slug) p.slug, p.title,
                  (SELECT count(DISTINCT tu.page_id) FROM template_uses tu
                   WHERE tu.wiki_id = p.wiki_id AND tu.template_slug = p.slug) AS "uses!"
           FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.deleted_at IS NULL
           ORDER BY p.slug, (COALESCE(p.locale, '') = $2) DESC
           LIMIT 1000"#,
        ctx.wiki.id,
        ctx.content_locale
    )
    .fetch_all(&state.db)
    .await?;
    let mut rows: Vec<_> = rows.into_iter().collect();
    rows.sort_by(|a, b| {
        b.uses
            .cmp(&a.uses)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    let rows = rows
        .into_iter()
        .map(|r| {
            row_with(
                r.title,
                href_of(ctx, "template", &r.slug),
                ctx.tn_with("system.used_on", r.uses, &[]),
                (r.uses == 0).then(|| ctx.t("system.unused")),
                None,
            )
        })
        .collect();
    Ok(list(rows))
}

/// Who edited in the last 30 days, most edits first.
async fn active_users(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT u.username, count(*) AS "edits!", max(r.created_at) AS "last!"
           FROM revisions r
           JOIN pages p ON p.id = r.page_id
           JOIN users u ON u.id = r.author_id
           WHERE p.wiki_id = $1 AND r.created_at > now() - interval '30 days'
           GROUP BY u.username
           ORDER BY count(*) DESC, u.username
           LIMIT $2"#,
        ctx.wiki.id,
        LIST_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let rows = rows
        .into_iter()
        .map(|r| {
            let meta = ctx.tn_with("system.edits_last", r.edits, &[("when", &ctx.day(r.last))]);
            row(r.username.clone(), format!("/user/{}", r.username), meta)
        })
        .collect();
    Ok(list(rows))
}
