//! Discussions: topics about a page, or about one change to it, with
//! replies, at `/{slug}/talk`.
//!
//! A topic belongs to the page's address in every language. One about a
//! change keeps the revision, and may name one run of changes in its diff
//! and quote words from it; the topic then shows that diff above the
//! messages, the added words in green and the removed ones in red, so
//! everybody talks about the same thing.
//!
//! Anybody may read. Writing needs an account in good standing (the
//! `ReportSend` capability, as for reports). Authors edit and take down
//! their own messages; moderators take down anybody's, hide and lock topics.
//! The topic's author, patrollers and moderators mark it resolved. Everyone
//! in a topic hears about replies, the page's watchers about new topics, and
//! `@name` tells that person.
//!
//! Everything works without scripts, and messages render once: their HTML
//! is kept in the render cache by the hash of their text.

use std::collections::{HashMap, HashSet};

use axum::extract::{Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tracing::instrument;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::diff::{self, Row};
use crate::pages::{self, ENGINE_VERSION};
use crate::perm::Capability;
use crate::resolve::Ctx;

const TITLE_MIN: usize = 3;
const TITLE_MAX: usize = 150;
const BODY_MAX: usize = 10_000;
const QUOTE_MAX: usize = 1_000;
/// Messages one person may send in ten minutes, and topics in an hour.
const POSTS_PER_BURST: i64 = 20;
const TOPICS_PER_HOUR: i64 = 10;
const TOPICS_SHOWN: i64 = 50;
/// Unchanged lines kept around the run of changes a topic is about.
const CONTEXT_ROWS: usize = 2;
/// People `@named` in one message who hear about it.
const MENTIONS_MAX: usize = 10;

/// The page a discussion is about: its address and title.
struct Subject {
    namespace: &'static str,
    slug: String,
    path: String,
    title: String,
}

impl Subject {
    fn href(&self, ctx: &Ctx) -> String {
        ctx.link(&format!("/{}", self.path))
    }

    fn talk_href(&self, ctx: &Ctx) -> String {
        ctx.link(&format!("/{}/talk", self.path))
    }
}

async fn subject(state: &AppState, ctx: &Ctx, raw: &str) -> Result<Option<Subject>, AppError> {
    let path = raw.trim().to_lowercase();
    if !pages::slug_is_valid(&path) {
        return Ok(None);
    }
    let (namespace, slug) = pages::split_path(&path);
    if !matches!(namespace, "main" | "template" | "page_template" | "module") {
        return Ok(None);
    }
    let title = sqlx::query_scalar!(
        "SELECT title FROM pages
         WHERE wiki_id = $1 AND namespace = ($2::text)::page_namespace AND slug = $3
           AND deleted_at IS NULL AND current_revision_id IS NOT NULL
         ORDER BY (COALESCE(locale, '') = $4) DESC LIMIT 1",
        ctx.wiki.id,
        namespace,
        slug,
        ctx.content_locale
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(title.map(|title| Subject {
        namespace,
        slug: slug.to_string(),
        path: path.clone(),
        title,
    }))
}

fn may_post(ctx: &Ctx) -> bool {
    ctx.actor.can(Capability::ReportSend)
}

fn is_moderator(ctx: &Ctx) -> bool {
    ctx.actor.can(Capability::ReportHandle)
}

fn may_resolve(ctx: &Ctx, author: Option<Uuid>) -> bool {
    is_moderator(ctx)
        || ctx.actor.can(Capability::RevisionPatrol)
        || (author.is_some() && author == ctx.actor.user_id)
}

/// The topics open on a page, for the page's Discussion link.
pub(crate) async fn open_count(state: &AppState, ctx: &Ctx, path: &str) -> Result<i64, AppError> {
    let (namespace, slug) = pages::split_path(path);
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM threads
           WHERE wiki_id = $1 AND namespace = ($2::text)::page_namespace AND slug = $3
             AND status = 'open' AND hidden_at IS NULL"#,
        ctx.wiki.id,
        namespace,
        slug
    )
    .fetch_one(&state.db)
    .await?)
}

/// Topics about one revision, for the diff page.
pub(crate) async fn about_revision(
    state: &AppState,
    revision: Uuid,
) -> Result<Vec<(Uuid, String, String)>, AppError> {
    Ok(sqlx::query!(
        "SELECT id, title, status FROM threads
         WHERE revision_id = $1 AND hidden_at IS NULL ORDER BY created_at",
        revision
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r.title, r.status))
    .collect())
}

// ---------------------------------------------------------------------------
// A change
// ---------------------------------------------------------------------------

/// A revision at the subject's address, with the one before it, as the
/// diff a topic about it shows. `hunk` narrows it to one run of changes.
async fn change_view(
    state: &AppState,
    ctx: &Ctx,
    subject: &Subject,
    revision: Uuid,
    hunk: Option<i32>,
) -> Result<Option<minijinja::Value>, AppError> {
    let Some(row) = sqlx::query!(
        r#"SELECT r.id, r.body_md, r.summary, r.created_at, r.review_status, r.author_id,
                  u.username AS "author?", r.imported_author,
                  prev.id AS "parent_id?", prev.body_md AS "parent_body?"
           FROM revisions r
           JOIN pages p ON p.id = r.page_id
           LEFT JOIN users u ON u.id = r.author_id
           LEFT JOIN LATERAL (
             SELECT o.id, o.body_md FROM revisions o
             WHERE o.page_id = r.page_id AND (o.created_at, o.id) < (r.created_at, r.id)
             ORDER BY o.created_at DESC, o.id DESC LIMIT 1
           ) prev ON true
           WHERE r.id = $1 AND p.wiki_id = $2 AND p.namespace = ($3::text)::page_namespace
             AND p.slug = $4"#,
        revision,
        ctx.wiki.id,
        subject.namespace,
        subject.slug
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(None);
    };
    if !crate::review::may_see(ctx, &row.review_status, row.author_id) {
        return Ok(None);
    }
    let before = row.parent_body.clone().unwrap_or_default();
    let computed = diff::diff_bodies(&before, &row.body_md);
    let rows: Vec<Row> = match hunk {
        Some(n) => hunk_rows(&computed.rows, n as usize),
        None => computed.rows.clone(),
    };
    let rows: Vec<minijinja::Value> = rows.iter().map(crate::history::row_value).collect();
    let diff_href = match row.parent_id {
        Some(parent) => format!("{}/diff?from={parent}&to={}", subject.href(ctx), row.id),
        None => format!("{}/rev/{}", subject.href(ctx), row.id),
    };
    Ok(Some(minijinja::context! {
        id => row.id.to_string(),
        author => row.author.or(row.imported_author),
        summary => row.summary,
        at => row.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        rows => rows,
        added => computed.added,
        removed => computed.removed,
        hunks => computed.hunks,
        hunk_list => (1..=computed.hunks.min(50)).collect::<Vec<_>>(),
        hunk => hunk,
        diff_href => diff_href,
        first => row.parent_id.is_none(),
    }))
}

/// One run of changes from a diff, with a little unchanged text around it.
fn hunk_rows(rows: &[Row], hunk: usize) -> Vec<Row> {
    let Some(start) = rows
        .iter()
        .position(|r| matches!(r, Row::Change { hunk: Some(h), .. } if *h == hunk))
    else {
        return rows.to_vec();
    };
    let mut end = start + 1;
    while end < rows.len() && matches!(rows[end], Row::Change { hunk: None, .. }) {
        end += 1;
    }
    let mut from = start;
    while from > 0 && start - from < CONTEXT_ROWS && matches!(rows[from - 1], Row::Context { .. }) {
        from -= 1;
    }
    let mut to = end;
    while to < rows.len() && to - end < CONTEXT_ROWS && matches!(rows[to], Row::Context { .. }) {
        to += 1;
    }
    rows[from..to].to_vec()
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// `@name`s in a message, lowercased, outside code.
fn mentioned(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    for_each_mention(body, |name| {
        if !found.iter().any(|n: &String| n == name) {
            found.push(name.to_string());
        }
    });
    found
}

/// Calls `f` with each `@name` outside code spans and fences.
fn for_each_mention(body: &str, mut f: impl FnMut(&str)) {
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for (n, part) in line.split('`').enumerate() {
            if n % 2 == 1 {
                continue;
            }
            let bytes = part.as_bytes();
            for (i, _) in part.match_indices('@') {
                let before_ok =
                    i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'/');
                let name: String = part[i + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                    .collect::<String>()
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if before_ok && crate::auth::username::is_valid(&name) {
                    f(&name);
                }
            }
        }
    }
}

/// The message's text with each `@name` of a real person made a link.
fn link_mentions(body: &str, people: &HashSet<String>) -> String {
    if people.is_empty() {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len() + 32);
    let mut in_fence = false;
    for line in body.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence {
            out.push_str(line);
            continue;
        }
        for (n, part) in line.split('`').enumerate() {
            if n > 0 {
                out.push('`');
            }
            if n % 2 == 1 {
                out.push_str(part);
                continue;
            }
            let mut rest = part;
            while let Some(at) = rest.find('@') {
                let before = &rest[..at];
                let before_ok = before
                    .chars()
                    .last()
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '/'));
                let name: String = rest[at + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                    .collect::<String>()
                    .trim_end_matches('.')
                    .to_string();
                out.push_str(before);
                if before_ok && people.contains(&name.to_ascii_lowercase()) {
                    out.push_str(&format!("[@{name}](/user/{})", name.to_ascii_lowercase()));
                    rest = &rest[at + 1 + name.len()..];
                } else {
                    out.push('@');
                    rest = &rest[at + 1..];
                }
            }
            out.push_str(rest);
        }
    }
    out
}

/// Which of `names` are people here.
async fn existing_users(
    db: &sqlx::PgPool,
    names: &[String],
) -> Result<HashMap<String, Uuid>, AppError> {
    if names.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(sqlx::query!(
        "SELECT id, username FROM users WHERE username = ANY($1)",
        names
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| (r.username, r.id))
    .collect())
}

/// Messages as HTML, from the render cache where they were rendered before.
async fn render_bodies(
    state: &AppState,
    ctx: &Ctx,
    bodies: &[String],
) -> Result<Vec<String>, AppError> {
    let mut names: Vec<String> = Vec::new();
    for body in bodies {
        for name in mentioned(body) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    let people: HashSet<String> = existing_users(&state.db, &names)
        .await?
        .into_keys()
        .collect();
    let texts: Vec<String> = bodies.iter().map(|b| link_mentions(b, &people)).collect();
    let keys: Vec<Vec<u8>> = texts
        .iter()
        .map(|t| naw_markdown::content_hash(t))
        .collect();
    let cached: HashMap<Vec<u8>, String> = sqlx::query!(
        "SELECT content_hash, html FROM render_cache
         WHERE wiki_id = $1 AND renderer_version = $2 AND content_hash = ANY($3)",
        ctx.wiki.id,
        naw_markdown::RENDERER_VERSION,
        &keys
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.content_hash, r.html))
    .collect();
    let mut out = Vec::with_capacity(texts.len());
    for (text, key) in texts.iter().zip(keys) {
        let html = match cached.get(&key) {
            Some(html) => html.clone(),
            None => {
                let html = naw_markdown::render_html(text);
                sqlx::query!(
                    "INSERT INTO render_cache (wiki_id, content_hash, renderer_version, html)
                     VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                    ctx.wiki.id,
                    key,
                    naw_markdown::RENDERER_VERSION,
                    html
                )
                .execute(&state.db)
                .await?;
                html
            }
        };
        out.push(html);
    }
    let joined = out.join("\u{1}");
    let joined = crate::emotes::expand(state, ctx.wiki.id, joined).await?;
    let joined = crate::redlinks::expand(state, ctx, joined).await?;
    Ok(joined.split('\u{1}').map(str::to_string).collect())
}

/// Whether this person may send another message now.
async fn within_rate(state: &AppState, me: Uuid, topic: bool) -> Result<bool, AppError> {
    let row = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM posts WHERE author_id = $1
                AND created_at > now() - interval '10 minutes') AS "posts!",
             (SELECT count(*) FROM threads WHERE author_id = $1
                AND created_at > now() - interval '1 hour') AS "topics!""#,
        me
    )
    .fetch_one(&state.db)
    .await?;
    Ok(row.posts < POSTS_PER_BURST && (!topic || row.topics < TOPICS_PER_HOUR))
}

/// Tells people about a message: `who` to tell, each once, never the writer.
async fn tell(
    state: &AppState,
    ctx: &Ctx,
    who: &[(Uuid, &'static str)],
    title: &str,
    link: &str,
    text: &str,
) {
    let mut told = HashSet::new();
    for (user, kind) in who {
        if Some(*user) == ctx.actor.user_id || !told.insert(*user) {
            continue;
        }
        crate::notify::push(
            &state.db,
            *user,
            ctx.wiki.id,
            &crate::notify::Note {
                kind,
                actor_id: ctx.actor.user_id,
                title,
                link,
                note: Some(text),
            },
        )
        .await;
    }
}

/// `@name`d people a message tells, at most a few.
async fn mention_targets(
    state: &AppState,
    body: &str,
) -> Result<Vec<(Uuid, &'static str)>, AppError> {
    let names: Vec<String> = mentioned(body).into_iter().take(MENTIONS_MAX).collect();
    Ok(existing_users(&state.db, &names)
        .await?
        .into_values()
        .map(|id| (id, "discussion_mention"))
        .collect())
}

fn check_body(ctx: &Ctx, body: &str) -> Result<(), String> {
    let chars = body.trim().chars().count();
    if chars == 0 {
        return Err(ctx.t("talk.body_empty"));
    }
    if chars > BODY_MAX {
        return Err(ctx.t_with("talk.body_long", &[("max", &BODY_MAX.to_string())]));
    }
    Ok(())
}

fn refuse(ctx: &Ctx, back: &str, message: &str) -> Response {
    pages::notice(
        ctx,
        StatusCode::UNPROCESSABLE_ENTITY,
        &ctx.t("talk.refused"),
        message,
        back,
        &ctx.t("talk.back"),
    )
    .unwrap_or_else(IntoResponse::into_response)
}

fn sign_in_or_refuse(ctx: &Ctx, back: &str) -> Response {
    if !ctx.actor.is_signed_in() {
        return pages::see_other(&format!("/login?next={}", pages::urlencode(back)));
    }
    pages::notice(
        ctx,
        StatusCode::FORBIDDEN,
        &ctx.t("error.not_allowed"),
        &ctx.t("talk.not_allowed"),
        back,
        &ctx.t("talk.back"),
    )
    .unwrap_or_else(IntoResponse::into_response)
}

// ---------------------------------------------------------------------------
// The list of topics
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    status: Option<String>,
    /// `page` for topics about the page as a whole, `changes` for those about a change.
    #[serde(default)]
    about: Option<String>,
    /// Open the form for a new topic.
    #[serde(default)]
    new: Option<u8>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    hunk: Option<i32>,
}

struct TopicFilter {
    status: &'static str,
    about: &'static str,
}

impl TopicFilter {
    fn of(query: &ListQuery) -> Self {
        Self {
            status: match query.status.as_deref() {
                Some("resolved") => "resolved",
                Some("all") => "all",
                _ => "open",
            },
            about: match query.about.as_deref() {
                Some("page") => "page",
                Some("changes") => "changes",
                _ => "all",
            },
        }
    }
}

/// Topics as the list shows them; `page` narrows to one page.
async fn topics(
    state: &AppState,
    ctx: &Ctx,
    page: Option<&Subject>,
    filter: &TopicFilter,
) -> Result<Vec<minijinja::Value>, AppError> {
    let moderator = is_moderator(ctx);
    let rows = sqlx::query!(
        r#"SELECT t.id, t.namespace::text AS "namespace!", t.slug, t.title, t.status, t.locked,
                  t.revision_id, t.hunk, t.replies, t.created_at, t.last_post_at,
                  t.hidden_at, u.username AS "author?",
                  (SELECT p.title FROM pages p WHERE p.wiki_id = t.wiki_id AND p.namespace = t.namespace
                     AND p.slug = t.slug AND p.deleted_at IS NULL
                   ORDER BY (COALESCE(p.locale, '') = $7) DESC LIMIT 1) AS "page_title?"
           FROM threads t
           LEFT JOIN users u ON u.id = t.author_id
           WHERE t.wiki_id = $1
             AND ($2::text IS NULL OR (t.namespace::text = $2 AND t.slug = $3))
             AND ($4 = 'all' OR t.status = $4)
             AND ($5 = 'all' OR ($5 = 'page' AND t.revision_id IS NULL)
                  OR ($5 = 'changes' AND t.revision_id IS NOT NULL))
             AND (t.hidden_at IS NULL OR $6)
           ORDER BY t.last_post_at DESC
           LIMIT $8"#,
        ctx.wiki.id,
        page.map(|p| p.namespace),
        page.map(|p| p.slug.as_str()).unwrap_or(""),
        filter.status,
        filter.about,
        moderator,
        ctx.content_locale,
        TOPICS_SHOWN
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
                locked => r.locked,
                hidden => r.hidden_at.is_some(),
                about_change => r.revision_id.is_some(),
                hunk => r.hunk,
                replies => r.replies,
                author => r.author,
                started => ctx.day(r.created_at),
                last => ctx.day(r.last_post_at),
                page_title => r.page_title.unwrap_or_else(|| path.clone()),
                page_href => ctx.link(&format!("/{path}")),
            })
        })
        .collect())
}

fn filter_links(ctx: &Ctx, base: &str, filter: &TopicFilter) -> minijinja::Value {
    let link = |status: &str, about: &str| format!("{base}?status={status}&about={about}");
    let statuses: Vec<minijinja::Value> = ["open", "resolved", "all"]
        .iter()
        .map(|s| {
            minijinja::context! {
                label => ctx.t(&format!("talk.status_{s}")),
                href => link(s, filter.about),
                current => *s == filter.status,
            }
        })
        .collect();
    let abouts: Vec<minijinja::Value> = ["all", "page", "changes"]
        .iter()
        .map(|a| {
            minijinja::context! {
                label => ctx.t(&format!("talk.about_{a}")),
                href => link(filter.status, a),
                current => *a == filter.about,
            }
        })
        .collect();
    minijinja::context! { statuses => statuses, abouts => abouts }
}

/// GET /{slug}/talk
#[instrument(skip(state, user, headers))]
pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = subject(&state, &ctx, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let filter = TopicFilter::of(&query);
    let revision = query.revision.as_deref().and_then(pages::parse_uuid);
    let change = match revision {
        Some(id) => change_view(&state, &ctx, &subject, id, query.hunk).await?,
        None => None,
    };
    if query.new.is_some() && !may_post(&ctx) {
        return Ok(sign_in_or_refuse(
            &ctx,
            &format!("{}?new=1", subject.talk_href(&ctx)),
        ));
    }
    let list = topics(&state, &ctx, Some(&subject), &filter).await?;
    render_list(
        &ctx,
        Some(&subject),
        list,
        &filter,
        query.new.is_some() || change.is_some(),
        change,
        None,
        "",
    )
}

#[allow(clippy::too_many_arguments)]
fn render_list(
    ctx: &Ctx,
    subject: Option<&Subject>,
    list: Vec<minijinja::Value>,
    filter: &TopicFilter,
    open_form: bool,
    change: Option<minijinja::Value>,
    problem: Option<String>,
    filled_title: &str,
) -> Result<Response, AppError> {
    let base = match subject {
        Some(s) => s.talk_href(ctx),
        None => ctx.link("/talk"),
    };
    let template = ctx
        .skin
        .env
        .get_template("talk.html")
        .map_err(pages::template_error)?;
    let title = match subject {
        Some(s) => ctx.t_with("talk.title", &[("page", &s.title)]),
        None => ctx.t("talk.all_title"),
    };
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => title,
                version => ENGINE_VERSION,
                page_title => subject.map(|s| s.title.clone()),
                page_href => subject.map(|s| s.href(ctx)),
                topics => list,
                filters => filter_links(ctx, &base, filter),
                action => subject.map(|s| s.talk_href(ctx)),
                may_post => may_post(ctx),
                signed_in => ctx.actor.is_signed_in(),
                login_href => format!("/login?next={}", pages::urlencode(&format!("{base}?new=1"))),
                open_form => open_form,
                change => change,
                problem => problem,
                filled_title => filled_title,
                title_max => TITLE_MAX,
                body_max => BODY_MAX,
            }
        })
        .map_err(pages::template_error)?;
    Ok(pages::html_response(html, &HeaderMap::new()))
}

/// GET /talk: every page's topics, newest first.
#[instrument(skip(state, user, headers))]
pub async fn recent(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let filter = TopicFilter::of(&query);
    let list = topics(&state, &ctx, None, &filter).await?;
    render_list(&ctx, None, list, &filter, false, None, None, "")
}

// ---------------------------------------------------------------------------
// A new topic
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct NewTopic {
    title: String,
    body: String,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    hunk: String,
    #[serde(default)]
    quote: String,
}

/// POST /{slug}/talk
#[instrument(skip(state, user, headers, form))]
pub async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<NewTopic>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = subject(&state, &ctx, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let back = subject.talk_href(&ctx);
    if !may_post(&ctx) {
        return Ok(sign_in_or_refuse(&ctx, &back));
    }
    let Some(me) = ctx.actor.user_id else {
        return Ok(sign_in_or_refuse(&ctx, &back));
    };
    let title = form.title.trim();
    let body = form.body.replace("\r\n", "\n");
    let revision = pages::parse_uuid(&form.revision);
    let hunk = form.hunk.trim().parse::<i32>().ok().filter(|h| *h > 0);
    let change = match revision {
        Some(id) => change_view(&state, &ctx, &subject, id, hunk).await?,
        None => None,
    };
    if revision.is_some() && change.is_none() {
        return Ok(crate::errors::not_found());
    }
    let problem = if !(TITLE_MIN..=TITLE_MAX).contains(&title.chars().count()) {
        Some(ctx.t_with(
            "talk.title_length",
            &[
                ("min", &TITLE_MIN.to_string()),
                ("max", &TITLE_MAX.to_string()),
            ],
        ))
    } else {
        check_body(&ctx, &body).err()
    };
    let problem = match problem {
        Some(p) => Some(p),
        None if !within_rate(&state, me, true).await? => Some(ctx.t("talk.too_fast")),
        None => None,
    };
    if let Some(problem) = problem {
        let list = topics(
            &state,
            &ctx,
            Some(&subject),
            &TopicFilter::of(&ListQuery::default()),
        )
        .await?;
        let mut response = render_list(
            &ctx,
            Some(&subject),
            list,
            &TopicFilter::of(&ListQuery::default()),
            true,
            change,
            Some(problem),
            title,
        )?;
        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        return Ok(response);
    }
    let quote: String = form.quote.trim().chars().take(QUOTE_MAX).collect();
    let thread_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "INSERT INTO threads (id, wiki_id, namespace, slug, revision_id, hunk, quote, title, author_id)
         VALUES ($1, $2, ($3::text)::page_namespace, $4, $5, $6, $7, $8, $9)",
        thread_id,
        ctx.wiki.id,
        subject.namespace,
        subject.slug,
        revision,
        hunk.filter(|_| revision.is_some()),
        (!quote.is_empty() && revision.is_some()).then_some(quote),
        title,
        me
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO posts (id, thread_id, author_id, body_md) VALUES ($1, $2, $3, $4)",
        Uuid::new_v4(),
        thread_id,
        me,
        body.trim()
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let link = format!("{}/{thread_id}", back);
    // The page's watchers, the author of the change, and anyone @named.
    let mut who: Vec<(Uuid, &'static str)> = mention_targets(&state, &body).await?;
    let watchers = sqlx::query_scalar!(
        "SELECT user_id FROM watches
         WHERE wiki_id = $1 AND namespace = ($2::text)::page_namespace AND slug = $3",
        ctx.wiki.id,
        subject.namespace,
        subject.slug
    )
    .fetch_all(&state.db)
    .await?;
    if let Some(id) = revision
        && let Some(Some(author)) =
            sqlx::query_scalar!("SELECT author_id FROM revisions WHERE id = $1", id)
                .fetch_optional(&state.db)
                .await?
    {
        who.push((author, "discussion_change"));
    }
    who.extend(watchers.into_iter().map(|u| (u, "discussion_new")));
    let note_title = format!("{}: {title}", subject.title);
    tell(&state, &ctx, &who, &note_title, &link, body.trim()).await;
    Ok(pages::see_other(&link))
}

// ---------------------------------------------------------------------------
// A topic
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, Default)]
pub struct TopicQuery {
    /// A message of one's own to edit in place.
    #[serde(default)]
    edit: Option<String>,
    /// A message to answer: it is quoted above the reply.
    #[serde(default)]
    reply: Option<String>,
}

struct Topic {
    id: Uuid,
    title: String,
    status: String,
    locked: bool,
    hidden: bool,
    author_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    hunk: Option<i32>,
    quote: Option<String>,
}

async fn load_topic(
    state: &AppState,
    ctx: &Ctx,
    subject: &Subject,
    raw: &str,
) -> Result<Option<Topic>, AppError> {
    let Some(id) = pages::parse_uuid(raw) else {
        return Ok(None);
    };
    let row = sqlx::query!(
        "SELECT id, title, status, locked, hidden_at, author_id, revision_id, hunk, quote FROM threads
         WHERE id = $1 AND wiki_id = $2 AND namespace = ($3::text)::page_namespace AND slug = $4",
        id,
        ctx.wiki.id,
        subject.namespace,
        subject.slug
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(row
        .filter(|r| r.hidden_at.is_none() || is_moderator(ctx))
        .map(|r| Topic {
            id: r.id,
            title: r.title,
            status: r.status,
            locked: r.locked,
            hidden: r.hidden_at.is_some(),
            author_id: r.author_id,
            revision_id: r.revision_id,
            hunk: r.hunk,
            quote: r.quote,
        }))
}

/// GET /{slug}/talk/{thread}
#[instrument(skip(state, user, headers))]
pub async fn show(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path((slug, thread)): Path<(String, String)>,
    Query(query): Query<TopicQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = subject(&state, &ctx, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let Some(topic) = load_topic(&state, &ctx, &subject, &thread).await? else {
        return Ok(crate::errors::not_found());
    };
    render_topic(&state, &ctx, &subject, &topic, &query, None, "").await
}

async fn render_topic(
    state: &AppState,
    ctx: &Ctx,
    subject: &Subject,
    topic: &Topic,
    query: &TopicQuery,
    problem: Option<String>,
    draft: &str,
) -> Result<Response, AppError> {
    let moderator = is_moderator(ctx);
    let me = ctx.actor.user_id;
    let rows = sqlx::query!(
        r#"SELECT p.id, p.author_id, p.reply_to, p.body_md, p.created_at, p.edited_at, p.hidden_at,
                  u.username AS "author?", u.display_name, u.avatar_key
           FROM posts p LEFT JOIN users u ON u.id = p.author_id
           WHERE p.thread_id = $1 ORDER BY p.created_at, p.id"#,
        topic.id
    )
    .fetch_all(&state.db)
    .await?;
    let visible: Vec<String> = rows
        .iter()
        .filter(|r| r.hidden_at.is_none() || moderator)
        .map(|r| r.body_md.clone())
        .collect();
    let mut rendered = render_bodies(state, ctx, &visible).await?.into_iter();
    let edit = query.edit.as_deref().and_then(pages::parse_uuid);
    let reply = query.reply.as_deref().and_then(pages::parse_uuid);
    let by_id: HashMap<Uuid, (Option<String>, String)> = rows
        .iter()
        .map(|r| (r.id, (r.author.clone(), excerpt(&r.body_md))))
        .collect();
    let topic_href = format!("{}/{}", subject.talk_href(ctx), topic.id);
    let posts: Vec<minijinja::Value> = rows
        .iter()
        .enumerate()
        .map(|(n, r)| {
            let hidden = r.hidden_at.is_some();
            let html = (!hidden || moderator).then(|| rendered.next().unwrap_or_default());
            let mine = me.is_some() && r.author_id == me;
            let answers = r.reply_to.and_then(|id| by_id.get(&id)).map(|(who, text)| {
                minijinja::context! { author => who.clone(), text => text.clone() }
            });
            minijinja::context! {
                id => r.id.to_string(),
                anchor => format!("post-{}", r.id),
                first => n == 0,
                author => r.author.clone(),
                display_name => r.display_name.clone(),
                avatar => r.avatar_key.as_deref().map(crate::media::url_for_key),
                at => r.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                day => ctx.day(r.created_at),
                edited => r.edited_at.is_some(),
                hidden => hidden,
                html => html,
                answers => answers,
                editing => mine && edit == Some(r.id) && !hidden,
                source => (mine && edit == Some(r.id)).then(|| r.body_md.clone()),
                may_edit => mine && !hidden,
                may_hide => (mine || moderator) && !hidden,
                may_unhide => moderator && hidden,
                reply_href => format!("{topic_href}?reply={}#reply", r.id),
                action => format!("{topic_href}/post/{}", r.id),
            }
        })
        .collect();
    let replying = reply.and_then(|id| {
        by_id
            .get(&id)
            .map(|(who, text)| (id, who.clone(), text.clone()))
    });
    let change = match topic.revision_id {
        Some(id) => change_view(state, ctx, subject, id, topic.hunk).await?,
        None => None,
    };
    let may_write = may_post(ctx) && (!topic.locked || moderator);
    let template = ctx
        .skin
        .env
        .get_template("thread.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => format!("{} · {}", topic.title, subject.title),
                version => ENGINE_VERSION,
                topic_title => topic.title.clone(),
                status => topic.status.clone(),
                locked => topic.locked,
                hidden => topic.hidden,
                page_title => subject.title.clone(),
                page_href => subject.href(ctx),
                talk_href => subject.talk_href(ctx),
                topic_href => topic_href.clone(),
                change => change,
                quote => topic.quote.clone(),
                posts => posts,
                may_write => may_write,
                signed_in => ctx.actor.is_signed_in(),
                login_href => format!("/login?next={}", pages::urlencode(&topic_href)),
                may_resolve => may_resolve(ctx, topic.author_id),
                moderator => moderator,
                replying => replying.map(|(id, who, text)| minijinja::context! {
                    id => id.to_string(), author => who, text => text,
                }),
                problem => problem,
                draft => draft,
                body_max => BODY_MAX,
            }
        })
        .map_err(pages::template_error)?;
    Ok(pages::html_response(html, &HeaderMap::new()))
}

/// A message's first words, for a quote above an answer.
fn excerpt(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut: String = flat.chars().take(160).collect();
    if flat.chars().count() > 160 {
        cut.push_str("...");
    }
    cut
}

#[derive(Debug, serde::Deserialize)]
pub struct ReplyForm {
    body: String,
    #[serde(default)]
    reply_to: String,
}

/// POST /{slug}/talk/{thread}
#[instrument(skip(state, user, headers, form))]
pub async fn reply(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path((slug, thread)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<ReplyForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = subject(&state, &ctx, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let Some(topic) = load_topic(&state, &ctx, &subject, &thread).await? else {
        return Ok(crate::errors::not_found());
    };
    let topic_href = format!("{}/{}", subject.talk_href(&ctx), topic.id);
    let (true, Some(me)) = (may_post(&ctx), ctx.actor.user_id) else {
        return Ok(sign_in_or_refuse(&ctx, &topic_href));
    };
    if topic.locked && !is_moderator(&ctx) {
        return Ok(refuse(&ctx, &topic_href, &ctx.t("talk.locked_note")));
    }
    let body = form.body.replace("\r\n", "\n");
    let problem = match check_body(&ctx, &body) {
        Err(p) => Some(p),
        Ok(()) if !within_rate(&state, me, false).await? => Some(ctx.t("talk.too_fast")),
        Ok(()) => None,
    };
    if let Some(problem) = problem {
        let query = TopicQuery {
            reply: Some(form.reply_to.clone()),
            ..TopicQuery::default()
        };
        let mut response =
            render_topic(&state, &ctx, &subject, &topic, &query, Some(problem), &body).await?;
        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        return Ok(response);
    }
    let reply_to = pages::parse_uuid(&form.reply_to);
    let post_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    let answered = sqlx::query_scalar!(
        "INSERT INTO posts (id, thread_id, author_id, reply_to, body_md)
         SELECT $1, $2, $3, (SELECT id FROM posts WHERE id = $4 AND thread_id = $2), $5
         RETURNING reply_to",
        post_id,
        topic.id,
        me,
        reply_to,
        body.trim()
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE threads SET replies = replies + 1, last_post_at = now() WHERE id = $1",
        topic.id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Everyone who wrote in the topic hears about the answer.
    let mut who: Vec<(Uuid, &'static str)> = mention_targets(&state, &body).await?;
    if let Some(id) = answered
        && let Some(Some(author)) =
            sqlx::query_scalar!("SELECT author_id FROM posts WHERE id = $1", id)
                .fetch_optional(&state.db)
                .await?
    {
        who.push((author, "discussion_reply"));
    }
    let writers = sqlx::query_scalar!(
        r#"SELECT DISTINCT author_id AS "author_id!" FROM posts
           WHERE thread_id = $1 AND author_id IS NOT NULL"#,
        topic.id
    )
    .fetch_all(&state.db)
    .await?;
    who.extend(writers.into_iter().map(|u| (u, "discussion_reply")));
    let link = format!("{topic_href}#post-{post_id}");
    let note_title = format!("{}: {}", subject.title, topic.title);
    tell(&state, &ctx, &who, &note_title, &link, body.trim()).await;
    Ok(pages::see_other(&link))
}

#[derive(Debug, serde::Deserialize)]
pub struct StatusForm {
    /// `resolved`, `open`, `lock`, `unlock`, `hide` or `unhide`.
    action: String,
}

/// POST /{slug}/talk/{thread}/status
#[instrument(skip(state, user, headers, form))]
pub async fn set_status(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path((slug, thread)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<StatusForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = subject(&state, &ctx, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let Some(topic) = load_topic(&state, &ctx, &subject, &thread).await? else {
        return Ok(crate::errors::not_found());
    };
    let topic_href = format!("{}/{}", subject.talk_href(&ctx), topic.id);
    let me = ctx.actor.user_id;
    let allowed = match form.action.as_str() {
        "resolved" | "open" => may_resolve(&ctx, topic.author_id),
        "lock" | "unlock" | "hide" | "unhide" => is_moderator(&ctx),
        _ => false,
    };
    if !allowed {
        return Ok(sign_in_or_refuse(&ctx, &topic_href));
    }
    sqlx::query!(
        "UPDATE threads SET
           status = CASE WHEN $2 IN ('resolved', 'open') THEN $2 ELSE status END,
           resolved_by = CASE WHEN $2 = 'resolved' THEN $3 WHEN $2 = 'open' THEN NULL ELSE resolved_by END,
           locked = CASE WHEN $2 = 'lock' THEN true WHEN $2 = 'unlock' THEN false ELSE locked END,
           hidden_at = CASE WHEN $2 = 'hide' THEN now() WHEN $2 = 'unhide' THEN NULL ELSE hidden_at END,
           hidden_by = CASE WHEN $2 = 'hide' THEN $3 WHEN $2 = 'unhide' THEN NULL ELSE hidden_by END
         WHERE id = $1",
        topic.id,
        form.action,
        me
    )
    .execute(&state.db)
    .await?;
    if form.action == "resolved"
        && let Some(author) = topic.author_id
    {
        let note_title = format!("{}: {}", subject.title, topic.title);
        tell(
            &state,
            &ctx,
            &[(author, "discussion_resolved")],
            &note_title,
            &topic_href,
            "",
        )
        .await;
    }
    let back = if form.action == "hide" {
        subject.talk_href(&ctx)
    } else {
        topic_href
    };
    Ok(pages::see_other(&back))
}

#[derive(Debug, serde::Deserialize)]
pub struct PostForm {
    /// `edit`, `hide` or `unhide`.
    action: String,
    #[serde(default)]
    body: String,
}

/// POST /{slug}/talk/{thread}/post/{post}
#[instrument(skip(state, user, headers, form))]
pub async fn post_action(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path((slug, thread, post)): Path<(String, String, String)>,
    headers: HeaderMap,
    Form(form): Form<PostForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = subject(&state, &ctx, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let Some(topic) = load_topic(&state, &ctx, &subject, &thread).await? else {
        return Ok(crate::errors::not_found());
    };
    let topic_href = format!("{}/{}", subject.talk_href(&ctx), topic.id);
    let Some(post_id) = pages::parse_uuid(&post) else {
        return Ok(crate::errors::not_found());
    };
    let Some(row) = sqlx::query!(
        "SELECT author_id, hidden_at FROM posts WHERE id = $1 AND thread_id = $2",
        post_id,
        topic.id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    let me = ctx.actor.user_id;
    let mine = me.is_some() && row.author_id == me;
    let moderator = is_moderator(&ctx);
    match form.action.as_str() {
        "edit" if mine && row.hidden_at.is_none() => {
            let body = form.body.replace("\r\n", "\n");
            if let Err(problem) = check_body(&ctx, &body) {
                return Ok(refuse(
                    &ctx,
                    &format!("{topic_href}?edit={post_id}#post-{post_id}"),
                    &problem,
                ));
            }
            sqlx::query!(
                "UPDATE posts SET body_md = $2, edited_at = now() WHERE id = $1",
                post_id,
                body.trim()
            )
            .execute(&state.db)
            .await?;
        }
        "hide" if (mine || moderator) && row.hidden_at.is_none() => {
            sqlx::query!(
                "UPDATE posts SET hidden_at = now(), hidden_by = $2 WHERE id = $1",
                post_id,
                me
            )
            .execute(&state.db)
            .await?;
        }
        "unhide" if moderator => {
            sqlx::query!(
                "UPDATE posts SET hidden_at = NULL, hidden_by = NULL WHERE id = $1",
                post_id
            )
            .execute(&state.db)
            .await?;
        }
        _ => return Ok(sign_in_or_refuse(&ctx, &topic_href)),
    }
    Ok(pages::see_other(&format!("{topic_href}#post-{post_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_found_outside_code_and_addresses() {
        assert_eq!(
            mentioned("Thanks @vai and @Akane. Mail me@example.com, see `@code` and /user/@x"),
            vec!["vai".to_string(), "akane".to_string()]
        );
        assert!(mentioned("```\n@vai\n```").is_empty());
    }

    #[test]
    fn mentions_of_real_people_become_links() {
        let people: HashSet<String> = ["vai".to_string()].into();
        assert_eq!(
            link_mentions("Hi @vai, and @nobody. `@vai`", &people),
            "Hi [@vai](/user/vai), and @nobody. `@vai`"
        );
    }

    #[test]
    fn a_hunk_keeps_its_rows_and_a_little_context() {
        let ctx_row = |n: usize| Row::Context {
            old_line: n,
            new_line: n,
            text: format!("line {n}"),
        };
        let change = |hunk: Option<usize>| Row::Change {
            old: None,
            new: None,
            hunk,
        };
        let rows = vec![
            ctx_row(1),
            ctx_row(2),
            ctx_row(3),
            change(Some(1)),
            ctx_row(4),
            ctx_row(5),
            ctx_row(6),
            change(Some(2)),
            change(None),
            ctx_row(7),
        ];
        let one = hunk_rows(&rows, 1);
        assert_eq!(one.len(), 5);
        assert_eq!(one[2], change(Some(1)));
        let two = hunk_rows(&rows, 2);
        assert_eq!(two, rows[5..10].to_vec());
        assert_eq!(hunk_rows(&rows, 9).len(), rows.len());
    }
}
