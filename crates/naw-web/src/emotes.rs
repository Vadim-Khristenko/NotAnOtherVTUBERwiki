//! 7TV emotes: sources, the local copies, and `:name:` in articles.
//!
//! Admins add 7TV users (their active set) or emote sets; those sources are
//! the allowlist. A sync downloads each file once into storage, within
//! `emote_budget_bytes`, so readers never contact 7TV. The renderer marks
//! unknown shortcodes (see `naw_markdown::EMOTE_OPEN`) and [`expand`] swaps
//! in the pictures after the render cache.

use std::collections::{HashMap, HashSet};

use axum::extract::{Extension, Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, ENGINE_VERSION, template_error};
use crate::perm::Capability;

const API: &str = "https://7tv.io/v3";
/// Largest emote file accepted.
const MAX_FILE: usize = 2 * 1024 * 1024;
/// Downloads running at once during a sync.
const IN_FLIGHT: usize = 6;
/// Emotes shown on the public list.
const LIST_MAX: i64 = 3000;

/// A 7TV source an admin entered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A 7TV user id: their active emote set.
    User(String),
    /// An emote set id, or "global".
    Set(String),
}

impl Source {
    fn kind(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Set(_) => "set",
        }
    }

    fn id(&self) -> &str {
        match self {
            Self::User(id) | Self::Set(id) => id,
        }
    }
}

/// Reads `https://7tv.app/users/{id}`, `https://7tv.app/emote-sets/{id}`,
/// `set:{id}`, `global`, or a bare user id.
pub fn parse_source(input: &str) -> Option<Source> {
    let raw = input.trim();
    let id_ok =
        |id: &str| (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric());
    if raw.eq_ignore_ascii_case("global") {
        return Some(Source::Set("global".into()));
    }
    if let Some(id) = raw.strip_prefix("set:") {
        return id_ok(id).then(|| Source::Set(id.to_string()));
    }
    let path = raw
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    if let Some(rest) = path.strip_prefix("7tv.app/") {
        let mut parts = rest.split(['/', '?', '#']);
        let (what, id) = (parts.next()?, parts.next()?);
        if !id_ok(id) {
            return None;
        }
        return match what {
            "users" => Some(Source::User(id.to_string())),
            "emote-sets" => Some(Source::Set(id.to_string())),
            _ => None,
        };
    }
    id_ok(raw).then(|| Source::User(raw.to_string()))
}

/// One emote as 7TV describes it.
#[derive(Debug, Clone, PartialEq)]
struct Remote {
    id: String,
    /// The name in the set, which may differ from the emote's own.
    name: String,
    animated: bool,
    url: String,
    width: u32,
    height: u32,
}

struct RemoteSet {
    label: String,
    set_id: String,
    emotes: Vec<Remote>,
}

/// Largest 7TV API answer read; a full set of 1000 emotes is well below it.
const JSON_MAX: usize = 8 * 1024 * 1024;

async fn get_json(state: &AppState, url: &str) -> Result<Value, String> {
    crate::fetch::get_json(state, url, JSON_MAX)
        .await
        .map_err(|err| match err {
            crate::fetch::FetchError::Status(404) => "7TV does not know this user or set".into(),
            err => format!("7TV: {err}"),
        })
}

/// The 2x WebP (sharp at text height, keeps animation), else any WebP.
fn pick_file(emote: &Value) -> Option<Remote> {
    let data = emote.get("data")?;
    let host = data.get("host")?;
    let base = host.get("url")?.as_str()?;
    let files = host.get("files")?.as_array()?;
    let is_webp = |f: &&Value| f.get("format").and_then(Value::as_str) == Some("WEBP");
    let file = files
        .iter()
        .find(|f| f.get("name").and_then(Value::as_str) == Some("2x.webp"))
        .or_else(|| files.iter().find(is_webp))?;
    let base = if base.starts_with("//") {
        format!("https:{base}")
    } else {
        base.to_string()
    };
    // Only 7TV's own CDN, whatever the API says.
    if !base.starts_with("https://cdn.7tv.app/") {
        return None;
    }
    Some(Remote {
        id: emote.get("id")?.as_str()?.to_string(),
        name: emote.get("name")?.as_str()?.to_string(),
        animated: data
            .get("animated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        url: format!("{base}/{}", file.get("name")?.as_str()?),
        width: file.get("width").and_then(Value::as_u64).unwrap_or(64) as u32,
        height: file.get("height").and_then(Value::as_u64).unwrap_or(64) as u32,
    })
}

async fn fetch_set(state: &AppState, source: &Source) -> Result<RemoteSet, String> {
    let (set_id, owner) = match source {
        Source::Set(id) => (id.clone(), None),
        Source::User(id) => {
            let user = get_json(state, &format!("{API}/users/{id}")).await?;
            let connections = user
                .get("connections")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // Their Twitch channel's set first, then any other connection's.
            let set_of = |c: &Value| {
                c.get("emote_set_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            let set = connections
                .iter()
                .find(|c| c.get("platform").and_then(Value::as_str) == Some("TWITCH"))
                .and_then(set_of)
                .or_else(|| connections.iter().find_map(set_of))
                .ok_or("this 7TV user has no active emote set")?;
            let name = user
                .get("display_name")
                .or_else(|| user.get("username"))
                .and_then(Value::as_str)
                .map(str::to_string);
            (set, name)
        }
    };
    let set = get_json(state, &format!("{API}/emote-sets/{set_id}")).await?;
    let set_name = set
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let label = match owner {
        Some(owner) => owner,
        None => set_name,
    };
    let emotes = set
        .get("emotes")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(pick_file).collect())
        .unwrap_or_default();
    Ok(RemoteSet {
        label,
        set_id: set
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&set_id)
            .to_string(),
        emotes,
    })
}

/// One downloaded, stored emote.
struct Stored {
    remote: Remote,
    key: String,
    size: i64,
}

async fn download(state: AppState, remote: Remote) -> Result<Stored, String> {
    let bytes = crate::fetch::get(&state, &remote.url, MAX_FILE)
        .await
        .map_err(|err| format!("download: {err}"))?
        .bytes;
    let Some(kind) = crate::media::sniff(&bytes) else {
        return Err("not an image".into());
    };
    let hash = hex::encode(Sha256::digest(&bytes));
    let key = format!("emotes/{hash}.{}", kind.ext);
    let size = bytes.len() as i64;
    if !state
        .storage
        .exists(&key)
        .await
        .map_err(|e| e.to_string())?
    {
        state
            .storage
            .put(&key, bytes)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(Stored { remote, key, size })
}

/// Bytes all emotes take, a file shared by several names counted once.
async fn budget_used(state: &AppState) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(size_bytes), 0)::bigint AS "used!"
           FROM (SELECT DISTINCT storage_key, size_bytes FROM emotes) files"#
    )
    .fetch_one(&state.db)
    .await?)
}

/// Starts a sync in the background; the admin page shows its progress.
pub fn spawn_sync(state: AppState, source_id: Uuid) {
    tokio::spawn(async move {
        if let Err(err) = sync(&state, source_id).await {
            tracing::warn!(error = %err, %source_id, "emote sync failed");
        }
    });
}

/// Brings the local copy of a source in line with 7TV: downloads new
/// emotes, removes gone ones, keeps unchanged ones.
async fn sync(state: &AppState, source_id: Uuid) -> Result<(), AppError> {
    let Some(claim) = sqlx::query!(
        "UPDATE emote_sources SET status = 'syncing', started_at = now(), error = NULL
         WHERE id = $1
           -- A sync still marked as running after 15 minutes died with its
           -- process, and another may start.
           AND (status <> 'syncing' OR started_at < now() - interval '15 minutes')
         RETURNING wiki_id, kind, ref",
        source_id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };
    let source = if claim.kind == "set" {
        Source::Set(claim.r#ref)
    } else {
        Source::User(claim.r#ref)
    };
    let outcome = run_sync(state, claim.wiki_id, source_id, &source).await;
    match outcome {
        Ok(()) => Ok(()),
        Err(message) => {
            sqlx::query!(
                "UPDATE emote_sources SET status = 'error', error = $2 WHERE id = $1",
                source_id,
                message
            )
            .execute(&state.db)
            .await?;
            Ok(())
        }
    }
}

async fn run_sync(
    state: &AppState,
    wiki_id: Uuid,
    source_id: Uuid,
    source: &Source,
) -> Result<(), String> {
    let db_err = |e: sqlx::Error| format!("database: {e}");
    let remote = fetch_set(state, source).await?;

    // The first source to claim a name keeps it.
    let taken: HashSet<String> = sqlx::query_scalar!(
        "SELECT name FROM emotes WHERE wiki_id = $1 AND source_id <> $2",
        wiki_id,
        source_id
    )
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?
    .into_iter()
    .collect();
    let have: HashMap<String, String> = sqlx::query!(
        "SELECT name, provider_id FROM emotes WHERE source_id = $1",
        source_id
    )
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?
    .into_iter()
    .map(|row| (row.name, row.provider_id))
    .collect();

    let mut skipped = 0;
    let mut wanted: Vec<Remote> = Vec::new();
    let mut seen = HashSet::new();
    for emote in remote.emotes {
        let usable = naw_markdown::is_emote_name(&emote.name)
            && !naw_markdown::is_unicode_shortcode(&emote.name)
            && !taken.contains(&emote.name)
            && seen.insert(emote.name.clone());
        if usable {
            wanted.push(emote);
        } else {
            skipped += 1;
        }
    }
    let keep: HashSet<String> = wanted
        .iter()
        .filter(|e| have.get(&e.name) == Some(&e.id))
        .map(|e| e.name.clone())
        .collect();
    let fetch: Vec<Remote> = wanted
        .iter()
        .filter(|e| !keep.contains(&e.name))
        .cloned()
        .collect();

    let budget = state.config.emote_budget_bytes as i64;
    let mut used = budget_used(state).await.map_err(|e| e.to_string())?;
    let mut stored: Vec<Stored> = Vec::new();
    let mut over_budget = false;
    let mut failed = 0;
    let mut queue = fetch.into_iter();
    let mut running = tokio::task::JoinSet::new();
    loop {
        while running.len() < IN_FLIGHT && !over_budget {
            let Some(next) = queue.next() else { break };
            running.spawn(download(state.clone(), next));
        }
        let Some(done) = running.join_next().await else {
            break;
        };
        match done {
            Ok(Ok(file)) => {
                if used + file.size > budget {
                    over_budget = true;
                    continue;
                }
                used += file.size;
                stored.push(file);
            }
            _ => failed += 1,
        }
    }
    let not_fetched = queue.len();

    let mut tx = state.db.begin().await.map_err(db_err)?;
    let current: Vec<String> = keep
        .iter()
        .cloned()
        .chain(stored.iter().map(|s| s.remote.name.clone()))
        .collect();
    sqlx::query!(
        "DELETE FROM emotes WHERE source_id = $1 AND NOT (name = ANY($2))",
        source_id,
        &current
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;
    for file in &stored {
        sqlx::query!(
            "INSERT INTO emotes (wiki_id, name, source_id, provider_id, storage_key, width, height, animated, size_bytes)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (wiki_id, name) DO UPDATE SET
               provider_id = EXCLUDED.provider_id, storage_key = EXCLUDED.storage_key,
               width = EXCLUDED.width, height = EXCLUDED.height,
               animated = EXCLUDED.animated, size_bytes = EXCLUDED.size_bytes
             WHERE emotes.source_id = EXCLUDED.source_id",
            wiki_id,
            file.remote.name,
            source_id,
            file.remote.id,
            file.key,
            file.remote.width as i32,
            file.remote.height as i32,
            file.remote.animated,
            file.size
        )
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    }
    let note = if over_budget {
        Some(format!(
            "storage budget reached: {not_fetched} more emotes were not downloaded"
        ))
    } else if failed > 0 {
        Some(format!(
            "{failed} files could not be downloaded; sync again to retry"
        ))
    } else {
        None
    };
    sqlx::query!(
        "UPDATE emote_sources SET status = 'ok', label = $2, set_id = $3, emote_count = $4,
                skipped = $5, error = $6, synced_at = now()
         WHERE id = $1",
        source_id,
        remote.label,
        remote.set_id,
        current.len() as i32,
        skipped,
        note
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;
    tx.commit().await.map_err(db_err)?;
    Ok(())
}

/// The names marked in rendered HTML, each once.
fn marked_names(html: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut rest = html;
    while let Some(at) = rest.find(naw_markdown::EMOTE_OPEN) {
        rest = &rest[at + naw_markdown::EMOTE_OPEN.len()..];
        if let Some(end) = rest.find(naw_markdown::EMOTE_CLOSE) {
            let name = rest[..end].trim_matches(':');
            if naw_markdown::is_emote_name(name) && seen.insert(name.to_string()) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// The picture for one emote; the stored file is 2x, drawn at half size.
fn emote_img(name: &str, key: &str, width: i32, height: i32) -> String {
    format!(
        "<img class=\"emote\" src=\"{}\" alt=\":{name}:\" title=\"{name}\" width=\"{}\" height=\"{}\" loading=\"lazy\" decoding=\"async\" />",
        crate::media::url_for_key(key),
        (width / 2).max(1),
        (height / 2).max(1)
    )
}

/// Replaces every marker of an emote this wiki has with its picture. One
/// substring search without markers, one query with them.
pub async fn expand(state: &AppState, wiki_id: Uuid, html: String) -> Result<String, AppError> {
    if !html.contains(naw_markdown::EMOTE_OPEN) {
        return Ok(html);
    }
    let names = marked_names(&html);
    if names.is_empty() {
        return Ok(html);
    }
    let found: HashMap<String, String> = sqlx::query!(
        "SELECT name, storage_key, width, height FROM emotes WHERE wiki_id = $1 AND name = ANY($2)",
        wiki_id,
        &names
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|row| {
        (
            row.name.clone(),
            emote_img(&row.name, &row.storage_key, row.width, row.height),
        )
    })
    .collect();
    if found.is_empty() {
        return Ok(html);
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html.as_str();
    while let Some(at) = rest.find(naw_markdown::EMOTE_OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + naw_markdown::EMOTE_OPEN.len()..];
        let Some(end) = after.find(naw_markdown::EMOTE_CLOSE) else {
            out.push_str(&rest[at..]);
            rest = "";
            break;
        };
        let inner = &after[..end];
        match found.get(inner.trim_matches(':')) {
            Some(img) => out.push_str(img),
            None => {
                out.push_str(naw_markdown::EMOTE_OPEN);
                out.push_str(inner);
                out.push_str(naw_markdown::EMOTE_CLOSE);
            }
        }
        rest = &after[end + naw_markdown::EMOTE_CLOSE.len()..];
    }
    out.push_str(rest);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    q: String,
}

/// Every emote of a wiki matching `query` (a substring of the name, any
/// case), with its source, sorted by source and name.
async fn find(
    state: &AppState,
    wiki_id: Uuid,
    query: &str,
) -> Result<Vec<(String, String, i32, i32, String)>, AppError> {
    let pattern = format!(
        "%{}%",
        query
            .trim()
            .chars()
            .take(64)
            .collect::<String>()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    Ok(sqlx::query!(
        r#"SELECT e.name, e.storage_key, e.width, e.height, s.label
           FROM emotes e JOIN emote_sources s ON s.id = e.source_id
           WHERE e.wiki_id = $1 AND e.name ILIKE $2
           ORDER BY s.created_at, lower(e.name), e.name LIMIT $3"#,
        wiki_id,
        pattern,
        LIST_MAX
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|row| {
        (
            row.name,
            row.storage_key,
            (row.width / 2).max(1),
            (row.height / 2).max(1),
            row.label,
        )
    })
    .collect())
}

/// GET /emotes: every emote of this wiki with its text, searchable with `?q=`.
pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let found = find(&state, ctx.wiki.id, &query.q).await?;
    let total = found.len();
    let mut groups: Vec<(String, Vec<minijinja::Value>)> = Vec::new();
    for (name, key, width, height, source) in found {
        let emote = minijinja::context! {
            name => name,
            url => crate::media::url_for_key(&key),
            width => width,
            height => height,
        };
        match groups.last_mut() {
            Some((label, list)) if *label == source => list.push(emote),
            _ => groups.push((source, vec![emote])),
        }
    }
    let groups: Vec<minijinja::Value> = groups
        .into_iter()
        .map(|(label, emotes)| minijinja::context! { label => label, emotes => emotes })
        .collect();
    let template = ctx
        .skin
        .env
        .get_template("emotes.html")
        .map_err(template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("emotes.title"),
                version => ENGINE_VERSION,
                groups => groups,
                total => total,
                query => query.q.trim(),
                can_manage => ctx.actor.can(Capability::WikiSettings),
            }
        })
        .map_err(template_error)?;
    Ok(pages::html_response(html, &headers))
}

/// GET /emotes.json: `[{"n": name, "u": url, "w": width, "h": height}]`, for
/// the editor's picker. The same for every reader, so it may be cached.
pub async fn list_json(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let list: Vec<Value> = find(&state, ctx.wiki.id, "")
        .await?
        .into_iter()
        .map(|(name, key, width, height, _)| {
            json!({ "n": name, "u": crate::media::url_for_key(&key), "w": width, "h": height })
        })
        .collect();
    Ok((
        [(axum::http::header::CACHE_CONTROL, "public, max-age=300")],
        axum::Json(list),
    )
        .into_response())
}

#[allow(clippy::result_large_err)]
async fn manage_gate(
    state: &AppState,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
) -> Result<crate::resolve::Ctx, Response> {
    let ctx = crate::admin::gate(state, headers, user).await?;
    if !ctx.actor.can(Capability::WikiSettings) {
        return Err((StatusCode::FORBIDDEN, "emotes need admin rights").into_response());
    }
    Ok(ctx)
}

#[derive(serde::Deserialize, Default)]
pub struct DoneQuery {
    #[serde(default)]
    done: Option<String>,
}

/// GET /admin/emotes
pub async fn admin_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<DoneQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(manage_gate(&state, &headers, user.as_ref()).await);
    let sources: Vec<minijinja::Value> = sqlx::query!(
        "SELECT id, kind, ref, label, status, error, emote_count, skipped, synced_at
         FROM emote_sources WHERE wiki_id = $1 ORDER BY created_at",
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|row| {
        let link = if row.kind == "set" {
            format!("https://7tv.app/emote-sets/{}", row.r#ref)
        } else {
            format!("https://7tv.app/users/{}", row.r#ref)
        };
        minijinja::context! {
            id => row.id.to_string(),
            kind => row.kind,
            label => if row.label.is_empty() { row.r#ref.clone() } else { row.label },
            link => link,
            status => row.status,
            error => row.error,
            count => row.emote_count,
            skipped => row.skipped,
            synced => row.synced_at.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()),
        }
    })
    .collect();
    let syncing = sources.iter().any(|s| {
        s.get_attr("status")
            .ok()
            .and_then(|v| v.as_str().map(|s| s == "syncing"))
            .unwrap_or(false)
    });
    let used = budget_used(&state).await?;
    let budget = state.config.emote_budget_bytes;
    let mb = |bytes: f64| format!("{:.1}", bytes / 1024.0 / 1024.0);
    crate::admin::render(
        &ctx,
        "emotes",
        &ctx.t("admin.emotes"),
        minijinja::context! {
            sources => sources,
            syncing => syncing,
            used_mb => mb(used as f64),
            budget_mb => mb(budget as f64),
            used_percent => if budget == 0 { 100 } else { ((used as f64 / budget as f64) * 100.0).round().min(100.0) as i64 },
            done => crate::pages::message_key(query.done.as_deref(), &[""]),
        },
    )
}

#[derive(serde::Deserialize)]
pub struct AddForm {
    source: String,
}

/// POST /admin/emotes/add
pub async fn add(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<AddForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(manage_gate(&state, &headers, user.as_ref()).await);
    let Some(source) = parse_source(&form.source) else {
        return Ok(pages::see_other("/admin/emotes?done=unreadable"));
    };
    let id = sqlx::query_scalar!(
        "INSERT INTO emote_sources (id, wiki_id, kind, ref, added_by)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (wiki_id, provider, kind, ref) DO NOTHING
         RETURNING id",
        Uuid::new_v4(),
        ctx.wiki.id,
        source.kind(),
        source.id(),
        ctx.actor.user_id
    )
    .fetch_optional(&state.db)
    .await?;
    let Some(id) = id else {
        return Ok(pages::see_other("/admin/emotes?done=already"));
    };
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "emotes.source_add",
            entity_type: "emote_source",
            entity_id: Some(id),
            meta: json!({ "kind": source.kind(), "ref": source.id() }),
        },
    )
    .await;
    spawn_sync(state.clone(), id);
    Ok(pages::see_other("/admin/emotes?done=added"))
}

/// POST /admin/emotes/{id}/sync
pub async fn resync(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(manage_gate(&state, &headers, user.as_ref()).await);
    let exists = sqlx::query_scalar!(
        "SELECT id FROM emote_sources WHERE id = $1 AND wiki_id = $2",
        id,
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?;
    if exists.is_none() {
        return Ok(crate::errors::not_found());
    }
    spawn_sync(state.clone(), id);
    Ok(pages::see_other("/admin/emotes?done=syncing"))
}

/// POST /admin/emotes/{id}/remove. Files stay in storage, shared by hash.
pub async fn remove(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(manage_gate(&state, &headers, user.as_ref()).await);
    let removed = sqlx::query!(
        "DELETE FROM emote_sources WHERE id = $1 AND wiki_id = $2 RETURNING kind, ref, label",
        id,
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?;
    if let Some(row) = removed {
        crate::audit::record_or_log(
            &state.db,
            crate::audit::Entry {
                wiki_id: Some(ctx.wiki.id),
                user_id: ctx.actor.user_id,
                action: "emotes.source_remove",
                entity_type: "emote_source",
                entity_id: Some(id),
                meta: json!({ "kind": row.kind, "ref": row.r#ref, "label": row.label }),
            },
        )
        .await;
    }
    Ok(pages::see_other("/admin/emotes?done=removed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_read_from_links_and_ids() {
        let user = "01FANHRSHR000E93E1VGQWFS53";
        assert_eq!(
            parse_source(&format!("https://7tv.app/users/{user}")),
            Some(Source::User(user.into()))
        );
        assert_eq!(parse_source(user), Some(Source::User(user.into())));
        assert_eq!(
            parse_source("https://7tv.app/emote-sets/01HJ4FERDR000F43C8J4WHCFB3?x=1"),
            Some(Source::Set("01HJ4FERDR000F43C8J4WHCFB3".into()))
        );
        assert_eq!(
            parse_source("set:abc123"),
            Some(Source::Set("abc123".into()))
        );
        assert_eq!(parse_source(" Global "), Some(Source::Set("global".into())));
        assert_eq!(parse_source("https://evil.example/users/x"), None);
        assert_eq!(parse_source("../../etc"), None);
        assert_eq!(parse_source(""), None);
    }

    #[test]
    fn only_7tv_cdn_files_are_taken() {
        let emote = |url: &str| {
            json!({ "id": "e1", "name": "catJAM", "data": { "animated": true, "host": {
                "url": url,
                "files": [
                    { "name": "1x.webp", "format": "WEBP", "width": 32, "height": 32 },
                    { "name": "2x.webp", "format": "WEBP", "width": 64, "height": 64 }
                ] } } })
        };
        let picked = pick_file(&emote("//cdn.7tv.app/emote/e1")).unwrap();
        assert_eq!(picked.url, "https://cdn.7tv.app/emote/e1/2x.webp");
        assert_eq!(
            (picked.width, picked.height, picked.animated),
            (64, 64, true)
        );
        assert!(pick_file(&emote("//evil.example/emote/e1")).is_none());
    }

    #[test]
    fn markers_are_found_once_each() {
        let open = naw_markdown::EMOTE_OPEN;
        let close = naw_markdown::EMOTE_CLOSE;
        let html = format!("<p>{open}:a:{close} and {open}:b:{close} {open}:a:{close}</p>");
        assert_eq!(marked_names(&html), vec!["a".to_string(), "b".to_string()]);
        let img = emote_img("KEKW", "emotes/ab.webp", 64, 48);
        assert!(
            img.contains(r#"src="/media/emotes/ab.webp""#) && img.contains(r#"width="32""#),
            "{img}"
        );
    }
}
