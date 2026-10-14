//! The search page: articles, categories, people, discussions, templates,
//! modules and files, each in its own tab, and all of them at once.
//!
//! "Everything" keeps the order a reader expects: an article whose title
//! matches comes first (the exact title before one that starts with the
//! words, before one that only contains them), then the categories, then the
//! people, then articles where the words are only in the text, each with up
//! to three passages from different sections, and the discussions last. A
//! tab shows one kind alone, with more of it and pages of results.
//!
//! Text is matched through the full text index (see `naw_core::search`):
//! stemmed words in each section, and the title, the addresses that redirect
//! to the page and its categories on the page itself. Titles, names and
//! usernames also match by part and by trigram, so a fragment or a typo still
//! finds them. Every query is bounded, and nothing here needs a script.

use std::collections::HashSet;

use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tracing::instrument;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::search::{clean_snippet, concordance, normalize, regconfig_for};
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, ENGINE_VERSION};
use crate::resolve::Ctx;

const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

/// Results of one kind on "Everything", and on a tab's page.
const ON_ALL: i64 = 5;
const ON_TAB: i64 = 20;
/// Pages found by their text on one page of "Everything".
const ON_TEXT: i64 = 10;
/// Sections of one article whose passages are shown.
const PASSAGES: i64 = 3;
/// Pieces of text considered before ranking.
const MATCHED_MAX: i64 = 4000;
/// Counts in the tabs stop here: "100+".
const COUNT_MAX: i64 = 100;
/// Trigram similarity a name needs to match without containing the words.
const SIMILAR: f32 = 0.3;

const SCOPES: [&str; 8] = [
    "all",
    "articles",
    "categories",
    "people",
    "discussions",
    "templates",
    "modules",
    "files",
];

/// The query as it arrives. Every field is text: a number that is not one,
/// or a flag spelled oddly, is read as absent instead of refusing the page.
#[derive(Debug, serde::Deserialize, Default)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    scope: Option<String>,
    /// `all` searches every language, otherwise the page's.
    #[serde(default)]
    lang: Option<String>,
    /// Only titles and names, not the text.
    #[serde(default)]
    titles: Option<String>,
    /// `recent` for the newest first.
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    page: Option<String>,
    /// `1` answers with the results alone, for the page's live search.
    #[serde(default)]
    fragment: Option<String>,
}

/// What the query asks for, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    /// The words, trimmed, capped, without control characters; `None` when
    /// nothing is left.
    text: Option<String>,
    scope: &'static str,
    page: i64,
    every_language: bool,
    titles_only: bool,
    recent: bool,
    fragment: bool,
}

/// Results pages a search goes to, at most.
const PAGES_MAX: i64 = 50;

impl Asked {
    fn of(query: &SearchQuery) -> Self {
        // A NUL reaches PostgreSQL as an error, other control characters
        // match nothing: neither belongs in a query.
        let clean: String = query.q.chars().filter(|c| !c.is_control()).collect();
        Asked {
            text: normalize(&clean),
            scope: query
                .scope
                .as_deref()
                .and_then(|s| SCOPES.iter().find(|x| **x == s).copied())
                .unwrap_or("all"),
            page: query
                .page
                .as_deref()
                .and_then(|p| p.trim().parse::<i64>().ok())
                .unwrap_or(1)
                .clamp(1, PAGES_MAX),
            every_language: query.lang.as_deref() == Some("all"),
            titles_only: query.titles.as_deref() == Some("1"),
            recent: query.sort.as_deref() == Some("recent"),
            fragment: query.fragment.as_deref() == Some("1"),
        }
    }

    /// The search page's address with the scope, a filter or the page changed.
    fn link(&self, ctx: &Ctx, change: Change) -> String {
        let mut next = self.clone();
        next.page = 1;
        match change {
            Change::Scope(scope) => next.scope = scope,
            Change::Page(page) => next.page = page,
            Change::EveryLanguage => next.every_language = !self.every_language,
            Change::TitlesOnly => next.titles_only = !self.titles_only,
            Change::Recent => next.recent = !self.recent,
            Change::Nothing => next.page = self.page,
        }
        let mut parts = vec![format!(
            "q={}",
            pages::urlencode(next.text.as_deref().unwrap_or(""))
        )];
        if next.scope != "all" {
            parts.push(format!("scope={}", next.scope));
        }
        if next.every_language {
            parts.push("lang=all".into());
        }
        if next.titles_only {
            parts.push("titles=1".into());
        }
        if next.recent {
            parts.push("sort=recent".into());
        }
        if next.page > 1 {
            parts.push(format!("page={}", next.page));
        }
        format!("{}?{}", ctx.link("/search"), parts.join("&"))
    }
}

/// One thing a link changes about the current search.
#[derive(Clone, Copy)]
enum Change {
    Scope(&'static str),
    Page(i64),
    EveryLanguage,
    TitlesOnly,
    Recent,
    Nothing,
}

/// One search, as the queries take it.
struct Ask<'a> {
    text: &'a str,
    like: String,
    config: &'static str,
    /// `None` searches every language.
    locale: Option<&'a str>,
    titles_only: bool,
    recent: bool,
}

/// Namespaces a scope covers.
fn namespaces(scope: &str) -> &'static [&'static str] {
    match scope {
        "templates" => &["template", "page_template"],
        "modules" => &["module"],
        _ => &["main"],
    }
}

/// `q` as an ILIKE pattern piece: its wildcards match themselves.
fn like(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// What one page of results holds.
struct Found {
    sections: Vec<minijinja::Value>,
    total: usize,
    more: bool,
    /// Pages were found, by title or text: the rest goes in the margin.
    pages: bool,
    /// Something besides pages was found.
    others: bool,
}

impl Found {
    fn push(
        &mut self,
        ctx: &Ctx,
        kind: &str,
        items: Vec<minijinja::Value>,
        more_href: Option<String>,
    ) {
        if items.is_empty() {
            return;
        }
        self.total += items.len();
        let page = matches!(kind, "titles" | "text");
        self.pages |= page;
        self.others |= !page;
        self.sections.push(minijinja::context! {
            kind => kind,
            side => !page,
            heading => ctx.t(&format!("search.section_{kind}")),
            items => items,
            more_href => more_href,
        });
    }
}

/// Takes `limit` of `items`, saying whether there were more.
fn cut<T>(mut items: Vec<T>, limit: i64) -> (Vec<T>, bool) {
    let more = items.len() as i64 > limit;
    items.truncate(limit as usize);
    (items, more)
}

/// Runs one page of a search. Only what the page shows is asked for: the
/// first page of "Everything" looks at titles, categories, people, the text
/// and discussions, a few of each; every later page, and every tab, at one
/// kind, a page at a time.
async fn find(
    state: &AppState,
    ctx: &Ctx,
    asked: &Asked,
    ask: &Ask<'_>,
) -> Result<Found, AppError> {
    let mut found = Found {
        sections: Vec::new(),
        total: 0,
        more: false,
        pages: false,
        others: false,
    };
    let offset = (asked.page - 1) * ON_TAB;
    let first = asked.page == 1;
    let more_link = |scope: &'static str| asked.link(ctx, Change::Scope(scope));
    match asked.scope {
        "all" => {
            let mut shown: HashSet<Uuid> = HashSet::new();
            if first {
                let (titled, more) = cut(
                    titled(state, ctx, ask, &["main"], ON_ALL + 1, 0).await?,
                    ON_ALL,
                );
                shown.extend(titled.iter().map(|(id, _)| *id));
                found.push(
                    ctx,
                    "titles",
                    titled.into_iter().map(|(_, v)| v).collect(),
                    more.then(|| more_link("articles")),
                );
                let (cats, more) = cut(categories(state, ctx, ask, ON_ALL + 1, 0).await?, ON_ALL);
                found.push(
                    ctx,
                    "categories",
                    cats,
                    more.then(|| more_link("categories")),
                );
                let (people, more) = cut(people(state, ctx, ask, ON_ALL + 1, 0).await?, ON_ALL);
                found.push(ctx, "people", people, more.then(|| more_link("people")));
            } else {
                // Later pages leave out what the first one showed by title.
                let titled = titled(state, ctx, ask, &["main"], ON_ALL, 0).await?;
                shown.extend(titled.iter().map(|(id, _)| *id));
            }
            if !ask.titles_only {
                let (text, more) = cut(
                    in_text(
                        state,
                        ctx,
                        ask,
                        &["main"],
                        &shown,
                        ON_TEXT + 1,
                        (asked.page - 1) * ON_TEXT,
                    )
                    .await?,
                    ON_TEXT,
                );
                found.more = more;
                found.push(ctx, "text", text, None);
            }
            if first {
                let (talk, more) = cut(discussions(state, ctx, ask, ON_ALL + 1, 0).await?, ON_ALL);
                found.push(
                    ctx,
                    "discussions",
                    talk,
                    more.then(|| more_link("discussions")),
                );
            }
        }
        "articles" | "templates" | "modules" => {
            let ns = namespaces(asked.scope);
            // The title matches stand on the first page; later pages still
            // leave them out of the text matches.
            let titled = titled(state, ctx, ask, ns, ON_TAB, 0).await?;
            let shown: HashSet<Uuid> = titled.iter().map(|(id, _)| *id).collect();
            let n_titled = titled.len();
            if first {
                found.push(
                    ctx,
                    "titles",
                    titled.into_iter().map(|(_, v)| v).collect(),
                    None,
                );
            }
            if !ask.titles_only {
                let (text, more) = cut(
                    in_text(state, ctx, ask, ns, &shown, ON_TAB + 1, offset).await?,
                    ON_TAB,
                );
                found.more = more;
                found.push(ctx, "text", text, None);
            } else {
                found.more = n_titled as i64 >= ON_TAB;
            }
        }
        "categories" => {
            let (items, more) = cut(
                categories(state, ctx, ask, ON_TAB + 1, offset).await?,
                ON_TAB,
            );
            found.more = more;
            found.push(ctx, "categories", items, None);
        }
        "people" => {
            let (items, more) = cut(people(state, ctx, ask, ON_TAB + 1, offset).await?, ON_TAB);
            found.more = more;
            found.push(ctx, "people", items, None);
        }
        "discussions" => {
            let (items, more) = cut(
                discussions(state, ctx, ask, ON_TAB + 1, offset).await?,
                ON_TAB,
            );
            found.more = more;
            found.push(ctx, "discussions", items, None);
        }
        _ => {
            let (items, more) = cut(files(state, ctx, ask, ON_TAB + 1, offset).await?, ON_TAB);
            found.more = more;
            found.push(ctx, "files", items, None);
        }
    }
    Ok(found)
}

fn ask_of<'a>(ctx: &'a Ctx, asked: &'a Asked) -> Option<Ask<'a>> {
    asked.text.as_deref().map(|text| Ask {
        text,
        like: like(text),
        config: regconfig_for(&ctx.content_locale),
        locale: (!asked.every_language).then_some(ctx.content_locale.as_str()),
        titles_only: asked.titles_only,
        recent: asked.recent,
    })
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
    let asked = Asked::of(&query);
    let ask = ask_of(&ctx, &asked);
    let found = match &ask {
        Some(ask) => find(&state, &ctx, &asked, ask).await?,
        None => Found {
            sections: Vec::new(),
            total: 0,
            more: false,
            pages: false,
            others: false,
        },
    };
    let tabs: Vec<minijinja::Value> = SCOPES
        .iter()
        .map(|id| {
            minijinja::context! {
                id => *id,
                label => ctx.t(&format!("search.scope_{id}")),
                current => *id == asked.scope,
                href => asked.link(&ctx, Change::Scope(id)),
            }
        })
        .collect();
    let toggle = |label: &str, on: bool, change: Change| {
        minijinja::context! {
            label => ctx.t(&format!("search.{label}")),
            on => on,
            href => asked.link(&ctx, change),
        }
    };
    let toggles = vec![
        toggle(
            "every_language",
            asked.every_language,
            Change::EveryLanguage,
        ),
        toggle("titles_only", asked.titles_only, Change::TitlesOnly),
        toggle("recent_first", asked.recent, Change::Recent),
    ];
    // Before a search, what the field understands, each trick a search to try.
    let tips: Vec<minijinja::Value> = ["stem", "phrase", "or", "not", "typo"]
        .iter()
        .map(|tip| {
            let q = ctx.t(&format!("search.tip_{tip}_q"));
            minijinja::context! {
                href => ctx.link(&format!("/search?q={}", pages::urlencode(&q))),
                q => q,
                says => ctx.t(&format!("search.tip_{tip}")),
            }
        })
        .collect();
    let text = asked.text.as_deref();
    let can_create = text
        .map(|t| crate::pages::slug_is_valid(&t.to_lowercase().replace(' ', "-")))
        .unwrap_or(false)
        && ctx.actor.can(crate::perm::Capability::PageCreate);
    let counts_href = text.map(|_| {
        let full = asked.link(&ctx, Change::Nothing);
        let query = full.split_once('?').map_or("", |(_, q)| q);
        format!("{}?{query}", ctx.link("/search/counts"))
    });
    let template = ctx
        .skin
        .env
        .get_template(if asked.fragment {
            "_search_results.html"
        } else {
            "search.html"
        })
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            // The page has its own field; the header's would be a second one.
            // Before the chrome, whose value it replaces.
            ..minijinja::context! { show_search => false, in_search => true },
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => match text {
                    Some(text) => ctx.t_with("search.title_for", &[("query", text)]),
                    None => ctx.t("search.heading"),
                },
                version => ENGINE_VERSION,
                query => text.unwrap_or(""),
                searched => text.is_some(),
                scope => asked.scope,
                tabs => tabs,
                toggles => toggles,
                tips => tips,
                sections => found.sections,
                // Pages in the main column and the rest beside them, when both.
                split => found.pages && found.others,
                total => found.total,
                page_no => asked.page,
                counts_href => counts_href,
                prev_href => (asked.page > 1).then(|| asked.link(&ctx, Change::Page(asked.page - 1))),
                next_href => (found.more && asked.page < PAGES_MAX).then(|| asked.link(&ctx, Change::Page(asked.page + 1))),
                create_href => can_create.then(|| ctx.link(&format!(
                    "/new?title={}",
                    pages::urlencode(text.unwrap_or(""))
                ))),
            }
        })
        .map_err(pages::template_error)?;
    let mut response = (StatusCode::OK, [HTML], html).into_response();
    // A fragment varies with nothing but its address.
    if asked.fragment {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("private, max-age=30"),
        );
    }
    Ok(response)
}

/// GET /search/counts: how many results each tab holds, capped, for the
/// page to fill in after it has shown the results. The counting runs apart
/// from the search, so a slow count never holds the results back.
#[instrument(skip(state, user, headers))]
pub async fn search_counts(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let asked = Asked::of(&query);
    let Some(ask) = ask_of(&ctx, &asked) else {
        return Ok((StatusCode::OK, axum::Json(serde_json::json!({}))).into_response());
    };
    let n = counts(&state, &ctx, &ask).await?;
    let shown = |n: i64| {
        if n >= COUNT_MAX {
            format!("{COUNT_MAX}+")
        } else {
            n.to_string()
        }
    };
    let body: serde_json::Map<String, serde_json::Value> = SCOPES[1..]
        .iter()
        .zip(n)
        .map(|(id, n)| (id.to_string(), serde_json::Value::String(shown(n))))
        .collect();
    Ok((
        StatusCode::OK,
        [(header::CACHE_CONTROL, "private, max-age=60")],
        axum::Json(body),
    )
        .into_response())
}

/// How many results each tab has, capped, in the order of `SCOPES` after "all".
async fn counts(state: &AppState, ctx: &Ctx, ask: &Ask<'_>) -> Result<Vec<i64>, AppError> {
    let row = sqlx::query!(
        r#"WITH q AS (SELECT websearch_to_tsquery($2::text::regconfig, $3) AS tsq,
                             websearch_to_tsquery('simple', $3) AS simple)
           SELECT
             (SELECT count(*) FROM (SELECT 1 FROM pages p, q WHERE p.wiki_id = $1 AND p.namespace = 'main'
                AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
                AND ($5::text IS NULL OR COALESCE(p.locale, '') = $5)
                AND (p.title ILIKE '%' || $4 || '%' OR p.search_vector @@ q.tsq
                     OR (NOT $7 AND EXISTS (SELECT 1 FROM search_chunks c WHERE c.page_id = p.id AND c.vector @@ q.tsq)))
                LIMIT $6) x) AS "articles!",
             (SELECT count(*) FROM (SELECT DISTINCT pc.category FROM page_categories pc
                WHERE pc.wiki_id = $1 AND (pc.name ILIKE '%' || $4 || '%' OR similarity(pc.name, $3) > $8)
                LIMIT $6) x) AS "categories!",
             (SELECT count(*) FROM (SELECT 1 FROM users u WHERE u.username NOT LIKE 'deleted-%'
                AND (u.username ILIKE '%' || $4 || '%' OR u.display_name ILIKE '%' || $4 || '%')
                AND EXISTS (SELECT 1 FROM wiki_memberships m WHERE m.user_id = u.id AND m.wiki_id = $1)
                LIMIT $6) x) AS "people!",
             (SELECT count(*) FROM (SELECT 1 FROM threads t, q WHERE t.wiki_id = $1 AND t.hidden_at IS NULL
                AND (t.title_vector @@ q.simple OR t.title ILIKE '%' || $4 || '%'
                     OR EXISTS (SELECT 1 FROM posts po WHERE po.thread_id = t.id AND po.hidden_at IS NULL
                                AND po.body_vector @@ q.simple))
                LIMIT $6) x) AS "discussions!",
             (SELECT count(*) FROM (SELECT 1 FROM pages p, q WHERE p.wiki_id = $1
                AND p.namespace IN ('template', 'page_template')
                AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
                AND (p.title ILIKE '%' || $4 || '%' OR p.slug ILIKE '%' || $4 || '%' OR p.search_vector @@ q.tsq)
                LIMIT $6) x) AS "templates!",
             (SELECT count(*) FROM (SELECT 1 FROM pages p, q WHERE p.wiki_id = $1 AND p.namespace = 'module'
                AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
                AND (p.title ILIKE '%' || $4 || '%' OR p.slug ILIKE '%' || $4 || '%' OR p.search_vector @@ q.tsq)
                LIMIT $6) x) AS "modules!",
             (SELECT count(*) FROM (SELECT 1 FROM media m WHERE m.wiki_id = $1 AND m.hidden_at IS NULL
                AND (m.name ILIKE '%' || $4 || '%' OR m.filename ILIKE '%' || $4 || '%')
                LIMIT $6) x) AS "files!""#,
        ctx.wiki.id,
        ask.config,
        ask.text,
        ask.like,
        ask.locale,
        COUNT_MAX,
        ask.titles_only,
        SIMILAR
    )
    .fetch_one(&state.db)
    .await?;
    Ok(vec![
        row.articles,
        row.categories,
        row.people,
        row.discussions,
        row.templates,
        row.modules,
        row.files,
    ])
}

/// Pages whose title or other name matches, best match first: the exact
/// title, then one that starts with the words, then one that contains
/// them, then a redirect's name, then a near spelling.
async fn titled(
    state: &AppState,
    ctx: &Ctx,
    ask: &Ask<'_>,
    namespaces: &[&str],
    limit: i64,
    offset: i64,
) -> Result<Vec<(Uuid, minijinja::Value)>, AppError> {
    let namespaces: Vec<String> = namespaces.iter().map(|s| s.to_string()).collect();
    let rows = sqlx::query!(
        r#"WITH q AS (SELECT websearch_to_tsquery($2::text::regconfig, $3) AS tsq)
           SELECT p.id, p.namespace::text AS "namespace!", p.slug, p.title, p.locale, p.updated_at,
                  (CASE WHEN lower(p.title) = lower($3) THEN 5
                        WHEN p.title ILIKE $4 || '%' THEN 4
                        WHEN p.title ILIKE '%' || $4 || '%' OR p.slug ILIKE '%' || $4 || '%' THEN 3
                        WHEN ts_filter(p.search_vector, '{a}') @@ q.tsq THEN 2
                        ELSE 0 END)::float8 + similarity(p.title, $3) AS "score!",
                  (lower(p.title) = lower($3)) AS "exact!",
                  ts_headline($2::text::regconfig, coalesce(c.body, ''), q.tsq,
                    'MaxWords=30, MinWords=12, ShortWord=3, MaxFragments=1, StartSel=' || chr(57344) || ', StopSel=' || chr(57345)
                  ) AS "snippet!"
           FROM pages p
           CROSS JOIN q
           LEFT JOIN search_chunks c ON c.page_id = p.id AND c.chunk_no = 1
           WHERE p.wiki_id = $1 AND p.namespace::text = ANY($5)
             AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
             AND ($6::text IS NULL OR COALESCE(p.locale, '') = $6)
             AND (p.title ILIKE '%' || $4 || '%' OR p.slug ILIKE '%' || $4 || '%'
                  OR p.title % $3 OR ts_filter(p.search_vector, '{a}') @@ q.tsq)
           ORDER BY CASE WHEN $7 THEN p.updated_at END DESC, 7 DESC, p.updated_at DESC
           LIMIT $8 OFFSET $9"#,
        ctx.wiki.id,
        ask.config,
        ask.text,
        ask.like,
        &namespaces,
        ask.locale,
        ask.recent,
        limit,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let path = pages::path_of(&r.namespace, &r.slug)?;
            Some((
                r.id,
                minijinja::context! {
                    href => page_href(ctx, &r.locale, &path),
                    title => r.title,
                    path => path.clone(),
                    namespace => r.namespace,
                    lang => (ask.locale.is_none()).then_some(r.locale),
                    exact => r.exact,
                    day => ctx.day(r.updated_at),
                    passages => vec![minijinja::context! {
                        snippet => clean_snippet(&r.snippet),
                    }],
                },
            ))
        })
        .collect())
}

/// A page's address in its own language.
fn page_href(ctx: &Ctx, locale: &Option<String>, path: &str) -> String {
    match locale.as_deref().filter(|l| !l.is_empty()) {
        Some(locale) => ctx.link_for(locale, &format!("/{path}")),
        None => ctx.link(&format!("/{path}")),
    }
}

/// Pages where the words are in the text, with up to three passages each,
/// from their best sections. Pages in `skip` were shown by title already.
#[allow(clippy::too_many_arguments)]
async fn in_text(
    state: &AppState,
    ctx: &Ctx,
    ask: &Ask<'_>,
    namespaces: &[&str],
    skip: &HashSet<Uuid>,
    limit: i64,
    offset: i64,
) -> Result<Vec<minijinja::Value>, AppError> {
    let namespaces: Vec<String> = namespaces.iter().map(|s| s.to_string()).collect();
    let skip: Vec<Uuid> = skip.iter().copied().collect();
    let rows = sqlx::query!(
        r#"WITH q AS (SELECT websearch_to_tsquery($2::text::regconfig, $3) AS tsq),
           matched AS (
             SELECT c.page_id, c.chunk_no, ts_rank_cd(c.vector, q.tsq) AS rank
             FROM search_chunks c, q
             WHERE c.wiki_id = $1 AND c.vector @@ q.tsq AND NOT (c.page_id = ANY($6))
             LIMIT $7
           ),
           ranked AS (
             SELECT m.page_id, max(m.rank) + 0.1 * ln(1 + count(*)) AS score, count(*) AS hits,
                    p.updated_at
             FROM matched m
             JOIN pages p ON p.id = m.page_id
             WHERE p.namespace::text = ANY($4) AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
               AND ($5::text IS NULL OR COALESCE(p.locale, '') = $5)
             GROUP BY m.page_id, p.updated_at
             ORDER BY CASE WHEN $8 THEN p.updated_at END DESC, score DESC
             LIMIT $9 OFFSET $10
           ),
           top AS (
             SELECT m.page_id, m.chunk_no,
                    row_number() OVER (PARTITION BY m.page_id ORDER BY m.rank DESC, m.chunk_no) AS n
             FROM matched m JOIN ranked r ON r.page_id = m.page_id
           )
           SELECT p.id, p.namespace::text AS "namespace!", p.slug, p.title, p.locale, p.updated_at,
                  r.score AS "score!", r.hits AS "hits!", t.n AS "n!", c.anchor, c.heading,
                  ts_headline($2::text::regconfig, c.body, q.tsq,
                    'MaxWords=32, MinWords=12, ShortWord=3, MaxFragments=2, FragmentDelimiter= … , StartSel=' || chr(57344) || ', StopSel=' || chr(57345)
                  ) AS "snippet!"
           FROM top t
           JOIN ranked r ON r.page_id = t.page_id
           JOIN pages p ON p.id = t.page_id
           JOIN search_chunks c ON c.page_id = t.page_id AND c.chunk_no = t.chunk_no
           CROSS JOIN q
           WHERE t.n <= $11
           ORDER BY CASE WHEN $8 THEN p.updated_at END DESC, r.score DESC, p.id, t.n"#,
        ctx.wiki.id,
        ask.config,
        ask.text,
        &namespaces,
        ask.locale,
        &skip,
        MATCHED_MAX,
        ask.recent,
        limit,
        offset,
        PASSAGES
    )
    .fetch_all(&state.db)
    .await?;
    // Rows come a passage each, a page's passages together: gather them.
    let mut pages_found: Vec<TextHit> = Vec::new();
    for r in rows {
        let Some(path) = pages::path_of(&r.namespace, &r.slug) else {
            continue;
        };
        if pages_found.last().is_none_or(|hit| hit.id != r.id) {
            pages_found.push(TextHit {
                id: r.id,
                namespace: r.namespace.clone(),
                href: page_href(ctx, &r.locale, &path),
                path,
                title: r.title.clone(),
                lang: ask.locale.is_none().then(|| r.locale.clone()).flatten(),
                day: ctx.day(r.updated_at),
                hits: r.hits,
                passages: Vec::new(),
            });
        }
        if let Some(hit) = pages_found.last_mut() {
            let href = (!r.anchor.is_empty()).then(|| format!("{}#{}", hit.href, r.anchor));
            hit.passages.push(minijinja::context! {
                section => (!r.heading.is_empty()).then(|| r.heading.clone()),
                href => href,
                line => concordance(&r.snippet),
                snippet => clean_snippet(&r.snippet),
            });
        }
    }
    Ok(pages_found
        .into_iter()
        .map(|hit| {
            minijinja::context! {
                href => hit.href,
                title => hit.title,
                path => hit.path,
                namespace => hit.namespace,
                lang => hit.lang,
                day => hit.day,
                hits => hit.hits,
                passages => hit.passages,
            }
        })
        .collect())
}

/// One page found by its text, with its passages.
struct TextHit {
    id: Uuid,
    namespace: String,
    href: String,
    path: String,
    title: String,
    lang: Option<String>,
    day: String,
    hits: i64,
    passages: Vec<minijinja::Value>,
}

/// Categories whose name matches, the exact name first, then by size.
async fn categories(
    state: &AppState,
    ctx: &Ctx,
    ask: &Ask<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT pc.category, min(pc.name) AS "name!", count(DISTINCT pc.page_id) AS "pages!",
                  bool_or(lower(pc.name) = lower($2)) AS "exact!"
           FROM page_categories pc
           JOIN pages p ON p.id = pc.page_id AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
           WHERE pc.wiki_id = $1 AND (pc.name ILIKE '%' || $3 || '%' OR pc.category ILIKE '%' || $3 || '%'
                                      OR similarity(pc.name, $2) > $4)
           GROUP BY pc.category
           ORDER BY 4 DESC, (min(pc.name) ILIKE $3 || '%') DESC, 3 DESC, pc.category
           LIMIT $5 OFFSET $6"#,
        ctx.wiki.id,
        ask.text,
        ask.like,
        SIMILAR,
        limit,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            minijinja::context! {
                href => ctx.link(&format!("/{}{}", crate::categories::PREFIX, r.category)),
                title => r.name,
                pages => r.pages,
                exact => r.exact,
            }
        })
        .collect())
}

/// People of this wiki whose username or display name matches.
async fn people(
    state: &AppState,
    ctx: &Ctx,
    ask: &Ask<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT u.username, u.display_name, u.avatar_key, m.role::text AS "role?",
                  (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id
                    WHERE r.author_id = u.id AND p.wiki_id = $1) AS "edits!"
           FROM users u
           JOIN wiki_memberships m ON m.user_id = u.id AND m.wiki_id = $1
           WHERE u.username NOT LIKE 'deleted-%'
             AND (u.username ILIKE '%' || $3 || '%' OR u.display_name ILIKE '%' || $3 || '%'
                  OR similarity(u.username, $2) > $4)
           ORDER BY (lower(u.username) = lower($2) OR lower(u.display_name) = lower($2)) DESC,
                    (u.username ILIKE $3 || '%' OR u.display_name ILIKE $3 || '%') DESC,
                    5 DESC, u.username
           LIMIT $5 OFFSET $6"#,
        ctx.wiki.id,
        ask.text,
        ask.like,
        SIMILAR,
        limit,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            minijinja::context! {
                href => format!("/user/{}", r.username),
                username => r.username,
                display_name => r.display_name,
                avatar => r.avatar_key.as_deref().map(crate::media::url_for_key),
                role => r.role.filter(|role| role != "registered"),
                edits => r.edits,
            }
        })
        .collect())
}

/// Topics whose name or messages hold the words, the latest active first.
async fn discussions(
    state: &AppState,
    ctx: &Ctx,
    ask: &Ask<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"WITH q AS (SELECT websearch_to_tsquery('simple', $2) AS tsq)
           SELECT t.id, t.namespace::text AS "namespace!", t.slug, t.title, t.status, t.replies, t.last_post_at,
                  best.body_md AS "body?",
                  (SELECT p.title FROM pages p WHERE p.wiki_id = t.wiki_id AND p.namespace = t.namespace
                     AND p.slug = t.slug AND p.deleted_at IS NULL
                   ORDER BY (COALESCE(p.locale, '') = $4) DESC LIMIT 1) AS "page_title?",
                  ts_headline('simple', coalesce(best.body_md, ''), q.tsq,
                    'MaxWords=28, MinWords=10, MaxFragments=1, StartSel=' || chr(57344) || ', StopSel=' || chr(57345)
                  ) AS "snippet!"
           FROM threads t
           CROSS JOIN q
           LEFT JOIN LATERAL (
             SELECT po.body_md FROM posts po
             WHERE po.thread_id = t.id AND po.hidden_at IS NULL
             ORDER BY (po.body_vector @@ q.tsq) DESC, po.created_at LIMIT 1
           ) best ON true
           WHERE t.wiki_id = $1 AND t.hidden_at IS NULL
             AND (t.title_vector @@ q.tsq OR t.title ILIKE '%' || $3 || '%'
                  OR EXISTS (SELECT 1 FROM posts po WHERE po.thread_id = t.id AND po.hidden_at IS NULL
                             AND po.body_vector @@ q.tsq))
           ORDER BY t.last_post_at DESC
           LIMIT $5 OFFSET $6"#,
        ctx.wiki.id,
        ask.text,
        ask.like,
        ctx.content_locale,
        limit,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let path = pages::path_of(&r.namespace, &r.slug)?;
            Some(minijinja::context! {
                href => ctx.link(&format!("/{path}/talk/{}", r.id)),
                title => r.title,
                status => r.status,
                replies => r.replies,
                day => ctx.day(r.last_post_at),
                page_title => r.page_title.unwrap_or_else(|| path.clone()),
                page_href => ctx.link(&format!("/{path}")),
                snippet => r.body.is_some().then(|| unmark(&clean_snippet(&r.snippet))),
            })
        })
        .collect())
}

/// A message's snippet without the Markdown signs it was written with:
/// `**bold**` reads as bold in the topic, not as stars in a result.
fn unmark(snippet: &str) -> String {
    let mut out = snippet.replace("**", "").replace("__", "").replace('`', "");
    // A link keeps its words: `[text](address)` is `text`.
    while let Some(open) = out.find("](") {
        let Some(close) = out[open..].find(')') else {
            break;
        };
        out.replace_range(open..open + close + 1, "");
    }
    out.replace(['[', ']'], "")
}

/// Files whose name matches, pictures with a preview.
async fn files(
    state: &AppState,
    ctx: &Ctx,
    ask: &Ask<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT m.name, m.kind, m.storage_key, m.filename, m.created_at
           FROM media m
           WHERE m.wiki_id = $1 AND m.hidden_at IS NULL
             AND (m.name ILIKE '%' || $2 || '%' OR m.filename ILIKE '%' || $2 || '%')
           ORDER BY (m.name ILIKE $2 || '%') DESC, m.created_at DESC
           LIMIT $3 OFFSET $4"#,
        ctx.wiki.id,
        ask.like,
        limit,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let prefix = crate::files::prefix_of(&r.kind);
            minijinja::context! {
                href => ctx.link(&format!("/{prefix}:{}", r.name)),
                title => r.name.clone(),
                kind => r.kind.clone(),
                preview => (r.kind == "image").then(|| crate::media::url_for_key(&r.storage_key)),
                uploaded_as => r.filename,
                day => ctx.day(r.created_at),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_in_a_query_match_themselves() {
        assert_eq!(like("50%_off"), "50\\%\\_off");
        assert_eq!(like("Filian"), "Filian");
    }

    fn ask(pairs: &[(&str, &str)]) -> Asked {
        let mut q = SearchQuery::default();
        for (k, v) in pairs {
            if *k == "q" {
                q.q = v.to_string();
                continue;
            }
            let v = Some(v.to_string());
            match *k {
                "scope" => q.scope = v,
                "lang" => q.lang = v,
                "titles" => q.titles = v,
                "sort" => q.sort = v,
                "page" => q.page = v,
                "fragment" => q.fragment = v,
                _ => {}
            }
        }
        Asked::of(&q)
    }

    #[test]
    fn a_query_is_cleaned_before_it_reaches_the_database() {
        // A NUL is an error in PostgreSQL; other controls match nothing.
        assert_eq!(ask(&[("q", "fil\0ian")]).text.as_deref(), Some("filian"));
        assert_eq!(
            ask(&[("q", "a\u{1}\u{7f}b\r\n")]).text.as_deref(),
            Some("ab")
        );
        assert_eq!(ask(&[("q", "   ")]).text, None);
        assert_eq!(ask(&[("q", "\0\0")]).text, None);
        // Capped, and cut on a character, not a byte.
        let long = ask(&[("q", &"ж".repeat(500))]).text.unwrap();
        assert_eq!(long.chars().count(), naw_core::search::QUERY_MAX);
        // Quotes and SQL are words to look for, nothing more.
        assert_eq!(
            ask(&[("q", "' OR 1=1; -- %_")]).text.as_deref(),
            Some("' OR 1=1; -- %_")
        );
        assert_eq!(like("' OR 1=1; -- %_"), "' OR 1=1; -- \\%\\_");
    }

    #[test]
    fn odd_parameters_fall_back_instead_of_refusing() {
        let a = ask(&[
            ("q", "x"),
            ("page", "abc"),
            ("scope", "../../etc"),
            ("sort", "DROP"),
        ]);
        assert_eq!((a.page, a.scope, a.recent), (1, "all", false));
        assert_eq!(ask(&[("q", "x"), ("page", "-5")]).page, 1);
        assert_eq!(ask(&[("q", "x"), ("page", "99999999999999999999")]).page, 1);
        assert_eq!(ask(&[("q", "x"), ("page", "7")]).page, 7);
        assert_eq!(ask(&[("q", "x"), ("page", "500")]).page, PAGES_MAX);
        assert_eq!(ask(&[("q", "x"), ("scope", "people")]).scope, "people");
        let flags = ask(&[
            ("q", "x"),
            ("lang", "all"),
            ("titles", "1"),
            ("sort", "recent"),
            ("fragment", "1"),
        ]);
        assert!(flags.every_language && flags.titles_only && flags.recent && flags.fragment);
        let off = ask(&[
            ("q", "x"),
            ("lang", "ALL"),
            ("titles", "yes"),
            ("fragment", "true"),
        ]);
        assert!(!off.every_language && !off.titles_only && !off.fragment);
    }

    #[test]
    fn a_message_snippet_loses_its_markdown() {
        assert_eq!(
            unmark("I think **<mark>Twitch</mark>** and [the lore](/lore) `code`"),
            "I think <mark>Twitch</mark> and the lore code"
        );
    }

    #[test]
    fn scopes_cover_their_namespaces() {
        assert_eq!(namespaces("templates"), &["template", "page_template"]);
        assert_eq!(namespaces("modules"), &["module"]);
        assert_eq!(namespaces("articles"), &["main"]);
    }
}
