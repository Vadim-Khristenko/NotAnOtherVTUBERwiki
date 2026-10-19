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
            ("modules", "code"),
            ("random", "arrows-shuffle"),
        ],
    ),
    (
        "activity",
        &[
            ("recent-changes", "activity"),
            ("discussions", "message-circle"),
            ("active-users", "users-group"),
            ("statistics", "chart-bar"),
            ("ping", "world"),
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

// How many rows each list shows is a limit: `recent_changes_shown`,
// `all_pages_shown`, `files_shown`, `system_list_shown`, `big_edit_bytes`.

/// The namespaces All pages can list, as the database spells them.
const LISTED_NAMESPACES: [&str; 5] = ["main", "category", "template", "page_template", "module"];

/// What a special page may be asked for in its query.
#[derive(Debug, Default)]
pub(crate) struct Query {
    /// All pages: start at the first title from here on.
    pub from: Option<String>,
    /// All pages and recent changes: which namespace.
    pub ns: Option<String>,
    /// Recent changes: how many days back.
    pub days: Option<i32>,
    /// Recent changes: leave out minor edits.
    pub hide_minor: bool,
    /// Recent changes: only the edits that started a page.
    pub only_new: bool,
    /// All pages: the most used first, for categories, templates and modules.
    pub by_uses: bool,
    /// Ping: the server's round trips to its parts, as JSON.
    pub probe_parts: bool,
}

/// The windows recent changes offers, in days.
const RECENT_DAYS: [i32; 4] = [1, 3, 7, 30];

/// GET /system and /system:name. `None` for a name that is not one.
pub(crate) async fn page(
    state: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    name: &str,
    query: &Query,
) -> Result<Option<Response>, AppError> {
    let (view, extra) = match name {
        "" => ("index", index(state, ctx).await?),
        "recent-changes" => ("recent-changes", recent_changes(state, ctx, query).await?),
        "all-pages" => ("all-pages", all_pages(state, ctx, query).await?),
        "files" => ("files", files(state, ctx).await?),
        "statistics" => ("statistics", statistics(state, ctx).await?),
        "random" => return random(state, ctx).await.map(Some),
        "discussions" => return Ok(Some(pages::see_other(&ctx.link("/talk")))),
        // Categories, templates and modules are tabs of All pages.
        "modules" | "categories" | "templates" => {
            return Ok(Some(pages::see_other(&all_pages_of(ctx, name))));
        }
        "wanted-categories" => ("wanted-categories", categories(state, ctx, true).await?),
        "uncategorized" => ("uncategorized", uncategorized(state, ctx).await?),
        "new-pages" => ("new-pages", new_pages(state, ctx).await?),
        "untranslated" => ("untranslated", untranslated(state, ctx).await?),
        "short-pages" => ("short-pages", by_size(state, ctx, true).await?),
        "long-pages" => ("long-pages", by_size(state, ctx, false).await?),
        "stale-pages" => ("stale-pages", stale_pages(state, ctx).await?),
        "unused-files" => ("unused-files", unused_files(state, ctx).await?),
        "active-users" => ("active-users", active_users(state, ctx).await?),
        "ping" => {
            let on = crate::ping::flags(&ctx.wiki.settings);
            if !on.page || (query.probe_parts && !on.parts) {
                return Ok(None);
            }
            if query.probe_parts {
                return Ok(Some(
                    (
                        StatusCode::OK,
                        [(header::CACHE_CONTROL, "no-store")],
                        axum::Json(crate::ping::parts(state).await),
                    )
                        .into_response(),
                ));
            }
            (
                "ping",
                minijinja::context! {
                    parts => on.parts,
                    echo_href => "/health",
                    parts_href => ctx.link("/system:ping?probe=parts"),
                },
            )
        }
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

/// The index: every special page with a live figure where one helps,
/// counted in one query.
async fn index(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let n = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL
                AND current_revision_id IS NOT NULL AND COALESCE(locale, '') = $2) AS "articles!",
             (SELECT count(DISTINCT category) FROM page_categories WHERE wiki_id = $1) AS "categories!",
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL
                AND current_revision_id IS NOT NULL AND COALESCE(locale, '') = $2
                AND created_at > now() - interval '7 days') AS "new_pages!",
             (SELECT count(*) FROM media WHERE wiki_id = $1 AND hidden_at IS NULL) AS "files!",
             (SELECT count(DISTINCT slug) FROM pages WHERE wiki_id = $1 AND namespace IN ('template', 'page_template')
                AND deleted_at IS NULL AND current_revision_id IS NOT NULL) AS "templates!",
             (SELECT count(DISTINCT slug) FROM pages WHERE wiki_id = $1 AND namespace = 'module'
                AND deleted_at IS NULL AND current_revision_id IS NOT NULL) AS "modules!",
             (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id WHERE p.wiki_id = $1
                AND r.review_status = 'accepted' AND r.created_at > now() - interval '1 day') AS "edits_day!",
             (SELECT count(*) FROM threads WHERE wiki_id = $1 AND status = 'open' AND hidden_at IS NULL) AS "open_topics!",
             (SELECT count(DISTINCT r.author_id) FROM revisions r JOIN pages p ON p.id = r.page_id
                WHERE p.wiki_id = $1 AND r.created_at > now() - interval '30 days') AS "active!",
             (SELECT count(*) FROM pages p WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
                AND p.current_revision_id IS NOT NULL AND COALESCE(p.locale, '') = $2
                AND NOT EXISTS (SELECT 1 FROM page_categories pc WHERE pc.page_id = p.id)) AS "uncategorized!",
             (SELECT count(DISTINCT pc.category) FROM page_categories pc JOIN pages p ON p.id = pc.page_id
                WHERE pc.wiki_id = $1 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
                AND NOT EXISTS (SELECT 1 FROM pages c WHERE c.wiki_id = $1 AND c.namespace = 'category'
                  AND c.slug = pc.category AND c.deleted_at IS NULL)) AS "wanted!",
             (SELECT count(DISTINCT p.slug) FROM pages p WHERE p.wiki_id = $1 AND p.namespace = 'main'
                AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
                AND NOT EXISTS (SELECT 1 FROM pages q WHERE q.wiki_id = p.wiki_id AND q.namespace = 'main'
                  AND q.slug = p.slug AND COALESCE(q.locale, '') = $2 AND q.deleted_at IS NULL)) AS "untranslated!""#,
        ctx.wiki.id,
        ctx.content_locale
    )
    .fetch_one(&state.db)
    .await?;
    let figure = |name: &str| -> Option<(String, i64)> {
        let (key, value) = match name {
            "all-pages" => ("articles", n.articles),
            "categories" => ("categories", n.categories),
            "new-pages" => ("new_pages", n.new_pages),
            "files" => ("files", n.files),
            "templates" => ("templates", n.templates),
            "modules" => ("modules", n.modules),
            "discussions" => ("open_topics", n.open_topics),
            "active-users" => ("active", n.active),
            "uncategorized" => ("uncategorized", n.uncategorized),
            "wanted-categories" => ("wanted", n.wanted),
            "untranslated" => ("untranslated", n.untranslated),
            _ => return None,
        };
        Some((
            ctx.tn_with(&format!("system.figure_{key}"), value, &[]),
            value,
        ))
    };
    let ping = crate::ping::flags(&ctx.wiki.settings).page;
    let groups: Vec<minijinja::Value> = GROUPS
        .iter()
        .map(|(group, entries)| {
            let entries: Vec<minijinja::Value> = entries
                .iter()
                .filter(|(name, _)| *name != "ping" || ping)
                .map(|(name, icon)| {
                    let key = name.replace('-', "_");
                    let href = match *name {
                        "discussions" => ctx.link("/talk"),
                        "modules" | "categories" | "templates" => all_pages_of(ctx, name),
                        _ => ctx.link(&format!("/system:{name}")),
                    };
                    let figure = figure(name);
                    minijinja::context! {
                        href => href,
                        icon => icon,
                        title => ctx.t(&format!("system.{key}")),
                        about => ctx.t(&format!("system.{key}_about")),
                        figure => figure.as_ref().map(|(_, n)| *n),
                        figure_label => figure.as_ref().map(|(label, _)| label.clone()),
                        // Work waiting: a maintenance list that is not empty.
                        attention => *group == "maintenance" && figure.is_some_and(|(_, n)| n > 0),
                    }
                })
                .collect();
            minijinja::context! {
                id => *group,
                title => ctx.t(&format!("system.group_{group}")),
                entries => entries,
            }
        })
        .collect();
    let today = vec![
        minijinja::context! {
            n => n.edits_day,
            label => ctx.tn_with("system.today_edits", n.edits_day, &[]),
            href => ctx.link("/system:recent-changes?days=1"),
        },
        minijinja::context! {
            n => n.new_pages,
            label => ctx.tn_with("system.today_new_pages", n.new_pages, &[]),
            href => ctx.link("/system:new-pages"),
        },
        minijinja::context! {
            n => n.open_topics,
            label => ctx.tn_with("system.today_topics", n.open_topics, &[]),
            href => ctx.link("/talk"),
        },
        minijinja::context! {
            n => n.active,
            label => ctx.tn_with("system.today_active", n.active, &[]),
            href => ctx.link("/system:active-users"),
        },
    ];
    Ok(minijinja::context! { groups => groups, today => today })
}

/// The tab of All pages a special page of that name became.
fn all_pages_of(ctx: &Ctx, name: &str) -> String {
    let ns = match name {
        "categories" => "category",
        "templates" => "template",
        _ => "module",
    };
    ctx.link(&format!("/system:all-pages?ns={ns}"))
}

/// Where a page of any namespace lives.
fn href_of(ctx: &Ctx, namespace: &str, slug: &str) -> String {
    href_in(ctx, "", namespace, slug)
}

/// [`href_of`] for a page stored in `locale`, in a list of every language.
fn href_in(ctx: &Ctx, locale: &str, namespace: &str, slug: &str) -> String {
    match pages::path_of(namespace, slug) {
        Some(path) => ctx.link_in(locale, &format!("/{path}")),
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

fn list(ctx: &Ctx, rows: Vec<minijinja::Value>) -> minijinja::Value {
    let shown = rows.len();
    minijinja::context! { rows => rows, list_max => ctx.limits.system_list_shown, at_limit => shown as i64 >= ctx.limits.system_list_shown }
}

async fn recent_changes(
    state: &AppState,
    ctx: &Ctx,
    query: &Query,
) -> Result<minijinja::Value, AppError> {
    let days = query.days.filter(|d| RECENT_DAYS.contains(d)).unwrap_or(7);
    let ns = query
        .ns
        .as_deref()
        .filter(|ns| LISTED_NAMESPACES.contains(ns));
    // The previous revision of each page comes from a subquery, not a join,
    // so the query plan (and sqlx's reading of it) stays the same.
    let rows = sqlx::query!(
        r#"SELECT r.id, r.created_at, r.summary, r.is_minor, r.bytes AS "bytes!", r.review_status,
                  p.title, p.slug, p.namespace::text AS "namespace!",
                  COALESCE(p.locale, '') AS "locale!",
                  (SELECT u.username FROM users u WHERE u.id = r.author_id) AS "author?",
                  (SELECT r2.id FROM revisions r2 WHERE r2.page_id = r.page_id
                     AND (r2.created_at, r2.id) < (r.created_at, r.id)
                     ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1) AS "prev_id?",
                  (SELECT r2.bytes FROM revisions r2 WHERE r2.page_id = r.page_id
                     AND (r2.created_at, r2.id) < (r.created_at, r.id)
                     ORDER BY r2.created_at DESC, r2.id DESC LIMIT 1) AS "prev_bytes?",
                  (SELECT t.id FROM threads t WHERE t.revision_id = r.id AND t.hidden_at IS NULL
                     ORDER BY t.created_at LIMIT 1) AS "thread?",
                  (SELECT count(*) FROM threads t WHERE t.revision_id = r.id AND t.hidden_at IS NULL) AS "threads!"
           FROM revisions r
           JOIN pages p ON p.id = r.page_id
           WHERE p.wiki_id = $1 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND (r.review_status = 'accepted' OR $3)
             AND r.created_at > now() - make_interval(days => $4)
             AND ($5::text IS NULL OR p.namespace::text = $5)
             AND (NOT $6 OR NOT r.is_minor)
             AND (NOT $7 OR NOT EXISTS (SELECT 1 FROM revisions r3 WHERE r3.page_id = r.page_id
                    AND (r3.created_at, r3.id) < (r.created_at, r.id)))
           ORDER BY r.created_at DESC, r.id DESC
           LIMIT $2"#,
        ctx.wiki.id,
        ctx.limits.recent_changes_shown,
        ctx.actor.can(Capability::RevisionPatrol),
        days,
        ns,
        query.hide_minor,
        query.only_new
    )
    .fetch_all(&state.db)
    .await?;
    let summary = sqlx::query!(
        r#"SELECT count(*) AS "edits!", count(DISTINCT r.page_id) AS "pages!",
                  count(DISTINCT r.author_id) AS "editors!",
                  (SELECT count(*) FROM threads t WHERE t.wiki_id = $1
                     AND t.last_post_at > now() - make_interval(days => $2)) AS "topics!"
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE p.wiki_id = $1 AND p.deleted_at IS NULL AND r.review_status = 'accepted'
             AND r.created_at > now() - make_interval(days => $2)"#,
        ctx.wiki.id,
        days
    )
    .fetch_one(&state.db)
    .await?;
    let here = |days: i32, ns: Option<&str>, hide_minor: bool, only_new: bool| {
        let mut parts = vec![format!("days={days}")];
        if let Some(ns) = ns {
            parts.push(format!("ns={ns}"));
        }
        if hide_minor {
            parts.push("hide_minor=1".into());
        }
        if only_new {
            parts.push("only_new=1".into());
        }
        format!("{}?{}", ctx.link("/system:recent-changes"), parts.join("&"))
    };
    let day_links: Vec<minijinja::Value> = RECENT_DAYS
        .iter()
        .map(|d| {
            minijinja::context! {
                label => ctx.tn_with("system.rc_days", i64::from(*d), &[]),
                href => here(*d, ns, query.hide_minor, query.only_new),
                current => *d == days,
            }
        })
        .collect();
    let ns_links: Vec<minijinja::Value> = std::iter::once(None)
        .chain(
            LISTED_NAMESPACES
                .iter()
                .filter(|n| **n != "category")
                .map(|n| Some(*n)),
        )
        .map(|n| {
            minijinja::context! {
                label => match n {
                    Some(n) => ctx.t(&format!("system.ns_{n}")),
                    None => ctx.t("system.rc_all_namespaces"),
                },
                href => here(days, n, query.hide_minor, query.only_new),
                current => n == ns,
            }
        })
        .collect();
    let filters = minijinja::context! {
        days => day_links,
        namespaces => ns_links,
        hide_minor => query.hide_minor,
        only_new => query.only_new,
        minor_href => here(days, ns, !query.hide_minor, query.only_new),
        new_href => here(days, ns, query.hide_minor, !query.only_new),
        summary => minijinja::context! {
            edits => summary.edits,
            pages => summary.pages,
            editors => summary.editors,
            topics => summary.topics,
        },
        days_n => days,
        talk_href => ctx.link("/talk"),
    };
    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            // Every link names the page's own language: the list has them all,
            // and a Russian revision does not exist under the English page.
            let link = |path: String| ctx.link_in(&row.locale, &path);
            let href = href_in(ctx, &row.locale, &row.namespace, &row.slug);
            let delta = row.bytes - row.prev_bytes.unwrap_or(0);
            // A diff lives under the page's own path, which for a profile is not here.
            let path = pages::path_of(&row.namespace, &row.slug);
            let diff = match (&path, row.prev_id) {
                (Some(path), Some(prev)) => {
                    Some(link(format!("/{path}/diff?from={prev}&to={}", row.id)))
                }
                _ => None,
            };
            // The topic about this change; with several, the page's topics.
            let talk = match (&path, row.thread) {
                (Some(path), Some(thread)) if row.threads == 1 => {
                    Some(link(format!("/{path}/talk/{thread}")))
                }
                (Some(path), Some(_)) => Some(link(format!("/{path}/talk"))),
                _ => None,
            };
            let history = path.map(|path| link(format!("/{path}/history")));
            minijinja::context! {
                title => row.title,
                href => href,
                diff => diff,
                talk => talk,
                talk_n => row.threads,
                talk_label => (row.threads > 0).then(|| ctx.tn_with("system.rc_talk", row.threads, &[])),
                history => history,
                author => row.author,
                summary => row.summary,
                is_minor => row.is_minor,
                is_new => row.prev_id.is_none(),
                delta => delta,
                delta_big => i64::from(delta.abs()) >= ctx.limits.big_edit_bytes,
                bar => size_bar(i64::from(delta)),
                locale => (row.locale != ctx.content_locale).then_some(row.locale),
                review => (row.review_status != "accepted").then(|| minijinja::context! {
                    status => row.review_status.clone(),
                    label => ctx.t(&format!("review.status_{}", row.review_status)),
                    open => format!("/admin/review/{}", row.id),
                }),
                day => ctx.day(row.created_at),
                time => row.created_at.format("%H:%M").to_string(),
            }
        })
        .collect();
    Ok(minijinja::context! {
        changes => items,
        recent_max => ctx.limits.recent_changes_shown,
        filters => filters,
    })
}

/// How wide a change's size bar is, in percent: on a log scale, so a
/// ten-byte fix shows and a five-megabyte import does not drown the rest.
fn size_bar(delta: i64) -> u32 {
    if delta == 0 {
        return 0;
    }
    let magnitude = (delta.unsigned_abs() as f64 + 1.0).log10();
    // 1 byte is about 8 %, a kilobyte about 75 %, ten kilobytes and more the whole bar.
    ((magnitude * 25.0).round() as u32).clamp(4, 100)
}

/// A share of the biggest, in percent, for a bar in a top list.
fn share(n: i64, max: i64) -> i64 {
    if max <= 0 {
        0
    } else {
        (n * 100 / max).clamp(1, 100)
    }
}

/// The letter a title is filed under.
fn letter_of(title: &str) -> String {
    title
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "#".to_string())
}

/// One line of All pages.
struct Listed {
    title: String,
    href: String,
    /// Pages in a category, pages that use a template or a module.
    uses: Option<i64>,
    /// Last edited, for articles.
    day: Option<String>,
    /// A word about what is missing: no description, not used.
    tag: Option<String>,
}

/// Every page of one namespace by title, a page at a time from `from`, or
/// for categories, templates and modules the most used first. Categories
/// include the ones only used, with no page of their own.
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
    let countable = matches!(ns, "category" | "template" | "module");
    let by_uses = countable && query.by_uses;
    let from: String = query
        .from
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let from = if by_uses { String::new() } else { from };
    let shown = ctx.limits.all_pages_shown as usize;
    let counts = sqlx::query!(
        r#"SELECT p.namespace::text AS "namespace!", count(DISTINCT p.slug) AS "n!"
           FROM pages p
           WHERE p.wiki_id = $1 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND p.namespace IN ('main', 'template', 'page_template', 'module')
             AND (p.namespace <> 'main' OR COALESCE(p.locale, '') = $2)
           GROUP BY p.namespace"#,
        ctx.wiki.id,
        ctx.content_locale
    )
    .fetch_all(&state.db)
    .await?;

    let mut letters: Vec<String>;
    let mut listed: Vec<Listed>;
    let category_total;
    if ns == "category" {
        // Few enough to sort and page here, and the used-only ones have no
        // row of their own to page through in SQL.
        let mut all: Vec<Listed> = category_entries(state, ctx, false)
            .await?
            .into_iter()
            .map(|(key, e)| Listed {
                href: crate::categories::href(ctx, &key),
                uses: Some(e.n),
                day: None,
                tag: (!e.described).then(|| ctx.t("system.no_description")),
                title: e.name,
            })
            .collect();
        category_total = all.len() as i64;
        all.sort_by_key(|l| l.title.to_lowercase());
        letters = all.iter().map(|l| letter_of(&l.title)).collect();
        if by_uses {
            all.sort_by_key(|l| std::cmp::Reverse(l.uses));
        } else {
            let from = from.to_lowercase();
            all.retain(|l| l.title.to_lowercase() >= from);
        }
        all.truncate(shown + 1);
        listed = all;
    } else {
        category_total = category_entries(state, ctx, false).await?.len() as i64;
        // Templates and modules live in every language at once; articles in one.
        let one_language = ns == "main";
        let rows = sqlx::query!(
            r#"SELECT s.title AS "title!", s.slug AS "slug!", s.updated_at AS "updated_at!", s.uses AS "uses?" FROM (
                 SELECT p.title, p.slug, p.updated_at,
                        CASE ($4::text)
                          WHEN 'template' THEN (SELECT count(DISTINCT tu.page_id) FROM template_uses tu
                                                WHERE tu.wiki_id = p.wiki_id AND tu.template_slug = p.slug)
                          WHEN 'module' THEN (SELECT count(DISTINCT mu.page_id) FROM module_uses mu
                                              WHERE mu.wiki_id = p.wiki_id AND mu.module_slug = p.slug)
                        END AS uses
                 FROM pages p
                 WHERE p.wiki_id = $1 AND p.namespace = ($4::text)::page_namespace
                   AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
                   AND (NOT $5 OR COALESCE(p.locale, '') = $2)
                   AND lower(p.title) >= lower($6)
               ) s
               ORDER BY CASE WHEN $7 THEN s.uses END DESC NULLS LAST, lower(s.title), s.slug
               LIMIT $3"#,
            ctx.wiki.id,
            ctx.content_locale,
            ctx.limits.all_pages_shown + 1,
            ns,
            one_language,
            from,
            by_uses
        )
        .fetch_all(&state.db)
        .await?;
        letters = sqlx::query_scalar!(
            r#"SELECT DISTINCT upper(left(p.title, 1)) AS "letter!" FROM pages p
               WHERE p.wiki_id = $1 AND p.namespace = ($3::text)::page_namespace
                 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL AND (NOT $4 OR COALESCE(p.locale, '') = $2)
               ORDER BY 1"#,
            ctx.wiki.id,
            ctx.content_locale,
            ns,
            one_language
        )
        .fetch_all(&state.db)
        .await?;
        // A template has one row per language; the list shows it once.
        let mut seen = std::collections::HashSet::new();
        listed = rows
            .into_iter()
            .filter(|row| seen.insert(row.slug.clone()))
            .map(|row| Listed {
                href: href_of(ctx, ns, &row.slug),
                tag: (row.uses == Some(0)).then(|| ctx.t("system.unused")),
                uses: row.uses,
                day: (ns == "main").then(|| ctx.day(row.updated_at)),
                title: row.title,
            })
            .collect();
    }
    letters.retain(|l| !l.trim().is_empty());
    letters.dedup();

    let here = |from: &str, by_uses: bool| {
        let mut href = ctx.link("/system:all-pages");
        let mut sep = '?';
        if ns != "main" {
            href.push_str(&format!("{sep}ns={ns}"));
            sep = '&';
        }
        if by_uses {
            href.push_str(&format!("{sep}sort=uses"));
        } else if !from.is_empty() {
            href.push_str(&format!("{sep}from={}", pages::urlencode(from)));
        }
        href
    };
    let has_more = listed.len() > shown;
    let next = (has_more && !by_uses).then(|| here(&listed[shown].title, false));
    listed.truncate(shown);
    let most = listed.iter().filter_map(|l| l.uses).max().unwrap_or(0);
    let items: Vec<minijinja::Value> = listed
        .into_iter()
        .map(|l| {
            minijinja::context! {
                letter => letter_of(&l.title),
                uses_label => l.uses.map(|n| match ns {
                    "category" => ctx.tn_with("category.members", n, &[]),
                    _ => ctx.tn_with("system.used_on", n, &[]),
                }),
                share => l.uses.map(|n| if n == 0 { 0 } else { share(n, most) }),
                uses => l.uses,
                day => l.day,
                tag => l.tag,
                href => l.href,
                title => l.title,
            }
        })
        .collect();
    let tabs: Vec<minijinja::Value> = LISTED_NAMESPACES
        .iter()
        .map(|id| {
            let n = if *id == "category" {
                category_total
            } else {
                counts
                    .iter()
                    .find(|c| c.namespace == *id)
                    .map_or(0, |c| c.n)
            };
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
        .map(|l| minijinja::context! { href => here(&l, false), letter => l })
        .collect();
    let total = if ns == "category" {
        category_total
    } else {
        counts.iter().find(|c| c.namespace == ns).map_or(0, |c| c.n)
    };
    Ok(minijinja::context! {
        pages => items,
        total_label => match ns {
            "category" => ctx.tn_with("system.figure_categories", total, &[]),
            "template" => ctx.tn_with("system.figure_templates", total, &[]),
            "module" => ctx.tn_with("system.figure_modules", total, &[]),
            _ => ctx.tn_with("system.pages_count", total, &[]),
        },
        total => total,
        tabs => tabs,
        letters => if by_uses { Vec::new() } else { letters },
        ns => ns,
        from => from,
        next => next,
        first => (!from.is_empty()).then(|| here("", false)),
        action => ctx.link("/system:all-pages"),
        countable => countable,
        by_uses => by_uses,
        by_title_href => here("", false),
        by_uses_href => here("", true),
    })
}

async fn files(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        "SELECT storage_key, name, kind, width, height FROM media
         WHERE wiki_id = $1 AND hidden_at IS NULL ORDER BY created_at DESC LIMIT $2",
        ctx.wiki.id,
        ctx.limits.files_shown
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
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL AND current_revision_id IS NOT NULL) AS "articles!",
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
                WHERE p.wiki_id = $1 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL) AS "text_bytes!""#,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?;
    let mb = |bytes: i64| format!("{:.1}", bytes as f64 / 1024.0 / 1024.0);
    let group = |id: &str, figures: Vec<(&str, String)>| {
        let items: Vec<minijinja::Value> = figures
            .into_iter()
            .map(|(key, value)| {
                minijinja::context! { label => ctx.t(&format!("system.stat_{key}")), value => value }
            })
            .collect();
        minijinja::context! { title => ctx.t(&format!("system.stat_group_{id}")), figures => items }
    };
    let groups = vec![
        group(
            "content",
            vec![
                ("articles", row.articles.to_string()),
                ("categories", row.categories.to_string()),
                ("templates", row.templates.to_string()),
                ("text_mb", mb(row.text_bytes)),
            ],
        ),
        group(
            "activity",
            vec![
                ("edits", row.edits.to_string()),
                ("edits_week", row.edits_week.to_string()),
                ("editors_month", row.editors_month.to_string()),
                ("members", row.members.to_string()),
            ],
        ),
        group(
            "media",
            vec![
                ("files", row.files.to_string()),
                ("file_mb", mb(row.file_bytes)),
                ("emotes", row.emotes.to_string()),
            ],
        ),
    ];

    // Edits a day for the last 30 days, the empty days included.
    let days = sqlx::query!(
        r#"SELECT d::date AS "day!", count(r.id) AS "edits!"
           FROM generate_series(current_date - 29, current_date, interval '1 day') d
           LEFT JOIN revisions r ON r.created_at >= d AND r.created_at < d + interval '1 day'
             AND r.review_status = 'accepted'
             AND EXISTS (SELECT 1 FROM pages p WHERE p.id = r.page_id AND p.wiki_id = $1)
           GROUP BY d ORDER BY d"#,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let peak = days.iter().map(|d| d.edits).max().unwrap_or(0).max(1);
    let bars: Vec<minijinja::Value> = days
        .iter()
        .map(|d| {
            minijinja::context! {
                day => d.day.format("%Y-%m-%d").to_string(),
                edits => d.edits,
                // Percent of the tallest day, at least a sliver for a day with edits.
                height => if d.edits == 0 { 0 } else { (d.edits * 100 / peak).max(3) },
            }
        })
        .collect();
    let month_edits: i64 = days.iter().map(|d| d.edits).sum();

    let editors = sqlx::query!(
        r#"SELECT u.username, u.display_name, count(*) AS "edits!"
           FROM revisions r JOIN pages p ON p.id = r.page_id JOIN users u ON u.id = r.author_id
           WHERE p.wiki_id = $1 AND r.review_status = 'accepted' AND r.created_at > now() - interval '30 days'
             AND u.username NOT LIKE 'deleted-%'
           GROUP BY u.id ORDER BY 3 DESC, u.username LIMIT 10"#,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let top = editors.first().map_or(0, |r| r.edits);
    let editors = editors
        .into_iter()
        .map(|r| {
            minijinja::context! {
                href => format!("/user/{}", r.username),
                name => r.display_name.unwrap_or_else(|| r.username.clone()),
                edits => r.edits,
                share => share(r.edits, top),
            }
        })
        .collect::<Vec<_>>();
    let busiest = sqlx::query!(
        r#"SELECT p.title, p.slug, p.namespace::text AS "namespace!",
                  COALESCE(p.locale, '') AS "locale!", count(*) AS "edits!"
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE p.wiki_id = $1 AND p.deleted_at IS NULL AND r.review_status = 'accepted'
             AND r.created_at > now() - interval '30 days'
           GROUP BY p.id ORDER BY 4 DESC, p.title LIMIT 10"#,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let top = busiest.first().map_or(0, |r| r.edits);
    let busiest = busiest
        .into_iter()
        .map(|r| {
            minijinja::context! {
                href => href_in(ctx, &r.locale, &r.namespace, &r.slug),
                name => r.title,
                edits => r.edits,
                share => share(r.edits, top),
            }
        })
        .collect::<Vec<_>>();
    Ok(minijinja::context! {
        stat_groups => groups,
        bars => bars,
        peak => peak,
        month_edits => month_edits,
        editors => editors,
        busiest => busiest,
    })
}

async fn random(state: &AppState, ctx: &Ctx) -> Result<Response, AppError> {
    let slug = sqlx::query_scalar!(
        "SELECT slug FROM pages
         WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL AND current_revision_id IS NOT NULL
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

/// A category as the lists show it.
struct CategoryEntry {
    name: String,
    n: i64,
    described: bool,
}

/// Every category by key with its size, or only those nobody described yet.
/// A level written only inside a longer name (Streams in Streams/ARG) is a
/// category too, with no pages of its own.
async fn category_entries(
    state: &AppState,
    ctx: &Ctx,
    wanted_only: bool,
) -> Result<std::collections::BTreeMap<String, CategoryEntry>, AppError> {
    let rows = sqlx::query!(
        r#"WITH used AS (
             SELECT pc.category, min(pc.name) AS name,
                    count(DISTINCT p.namespace::text || ':' || p.slug) AS n
             FROM page_categories pc JOIN pages p ON p.id = pc.page_id
             WHERE pc.wiki_id = $1 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
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
    let mut entries: std::collections::BTreeMap<String, CategoryEntry> =
        std::collections::BTreeMap::new();
    for r in rows {
        let levels =
            naw_markdown::categories::levels_in(&r.name, naw_markdown::categories::Shape::LOOSEST);
        let mut prefix = String::new();
        let mut shown = Vec::new();
        for level in &levels {
            if !prefix.is_empty() {
                prefix.push(':');
            }
            prefix.push_str(&level.key);
            shown.push(level.name.clone());
            if prefix != r.key && !wanted_only {
                entries.entry(prefix.clone()).or_insert(CategoryEntry {
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
            CategoryEntry {
                name,
                n: r.n,
                described: r.described,
            },
        );
    }
    Ok(entries)
}

/// The categories nobody described yet, each with a link to write it.
async fn categories(
    state: &AppState,
    ctx: &Ctx,
    wanted_only: bool,
) -> Result<minijinja::Value, AppError> {
    let may_create = ctx.actor.can(Capability::PageCreate);
    let rows = category_entries(state, ctx, wanted_only)
        .await?
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
    Ok(list(ctx, rows))
}

async fn uncategorized(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, p.updated_at FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND COALESCE(p.locale, '') = $2
             AND NOT EXISTS (SELECT 1 FROM page_categories pc WHERE pc.page_id = p.id)
           ORDER BY lower(p.title) LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        ctx.limits.system_list_shown
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
    Ok(list(ctx, rows))
}

async fn new_pages(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, p.created_at,
                  (SELECT u.username FROM revisions r JOIN users u ON u.id = r.author_id
                   WHERE r.page_id = p.id ORDER BY r.created_at, r.id LIMIT 1) AS "author?"
           FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND COALESCE(p.locale, '') = $2
           ORDER BY p.created_at DESC LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        ctx.limits.system_list_shown
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
    Ok(list(ctx, rows))
}

/// Articles with no version in the reader's language, from the one in the
/// wiki's language when there is one.
async fn untranslated(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (p.slug) p.slug, p.title, COALESCE(p.locale, '') AS "locale!"
           FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND NOT EXISTS (
               SELECT 1 FROM pages q
               WHERE q.wiki_id = p.wiki_id AND q.namespace = 'main' AND q.slug = p.slug
                 AND COALESCE(q.locale, '') = $2 AND q.deleted_at IS NULL)
           ORDER BY p.slug, (COALESCE(p.locale, '') = $3) DESC
           LIMIT $4"#,
        ctx.wiki.id,
        ctx.content_locale,
        ctx.wiki.default_locale,
        ctx.limits.system_list_shown
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
    Ok(list(ctx, rows.into_iter().map(|(_, r)| r).collect()))
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
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND COALESCE(p.locale, '') = $2
           ORDER BY CASE WHEN $3 THEN r.bytes ELSE -r.bytes END, lower(p.title)
           LIMIT $4"#,
        ctx.wiki.id,
        ctx.content_locale,
        shortest,
        ctx.limits.system_list_shown
    )
    .fetch_all(&state.db)
    .await?;
    let rows = rows
        .into_iter()
        .map(|r| row(r.title, href_of(ctx, "main", &r.slug), size(ctx, r.bytes)))
        .collect();
    Ok(list(ctx, rows))
}

async fn stale_pages(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, p.updated_at FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND COALESCE(p.locale, '') = $2
           ORDER BY p.updated_at, lower(p.title) LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        ctx.limits.system_list_shown
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
    Ok(list(ctx, rows))
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
        ctx.limits.files_shown
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
        ctx.limits.system_list_shown
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
    Ok(list(ctx, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_bar_grows_with_the_size_and_stays_in_bounds() {
        assert_eq!(size_bar(0), 0);
        assert!(size_bar(1) >= 4);
        assert!(size_bar(10) < size_bar(1000));
        assert_eq!(size_bar(i64::MAX), 100);
        assert_eq!(size_bar(i64::MIN), 100);
        assert_eq!(size_bar(-500), size_bar(500));
    }

    #[test]
    fn a_share_is_a_percent_of_the_first() {
        assert_eq!(share(5, 10), 50);
        assert_eq!(share(10, 10), 100);
        assert_eq!(share(1, 1000), 1);
        assert_eq!(share(5, 0), 0);
        assert_eq!(share(20, 10), 100);
    }

    #[test]
    fn a_title_is_filed_under_its_first_letter() {
        assert_eq!(letter_of("bao"), "B");
        assert_eq!(letter_of("  ёлка"), "Ё");
        assert_eq!(letter_of("!!!"), "#");
        assert_eq!(letter_of(""), "#");
    }
}
