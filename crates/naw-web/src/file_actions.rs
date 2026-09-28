//! What can be done to an uploaded file from its page.
//!
//! - A new version under the same name: a better quality or another format of
//!   the same kind of file. Every page that shows the file shows the new one,
//!   since `image:name` is looked up when a page renders. The uploader and
//!   curators and up may do it, within the upload allowance.
//! - Bringing an older version back, which adds it again as the newest, so
//!   the history only grows. Same people.
//! - Hiding and showing it again (moderators and up): readers get neither the
//!   file nor its page, and articles link to the page in its place.
//! - Deleting it with its history (admins): the stored bytes go too, unless
//!   another file, here or in another wiki, still has them.

use axum::extract::{Extension, Form, Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::media::{Refusal, Stored};
use crate::pages;
use crate::perm::{Actor, Capability};
use crate::resolve::Ctx;

/// What a version's note and a hiding reason may hold.
const NOTE_MAX: usize = 300;

/// The file a path names in this wiki.
pub(crate) struct Target {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub storage_key: String,
    pub uploader_id: Option<Uuid>,
    pub hidden: bool,
}

impl Target {
    /// `image:ferris.png`
    pub fn path(&self) -> String {
        format!("{}:{}", crate::files::prefix_of(&self.kind), self.name)
    }
}

pub(crate) async fn target(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    slug: &str,
) -> Result<Option<Target>, AppError> {
    let slug = slug.trim().to_lowercase();
    let Some((_, name)) = crate::files::split(&slug) else {
        return Ok(None);
    };
    Ok(sqlx::query_as!(
        Target,
        r#"SELECT id, name, kind, storage_key, uploader_id, (hidden_at IS NOT NULL) AS "hidden!"
           FROM media WHERE wiki_id = $1 AND name = $2"#,
        wiki_id,
        name
    )
    .fetch_optional(db)
    .await?)
}

/// Whether this reader may put up a new version or bring an old one back.
pub(crate) fn may_replace(actor: &Actor, file: &Target) -> bool {
    if file.hidden && !actor.can(Capability::PageDelete) {
        return false;
    }
    actor.can(Capability::RevisionPatrol)
        || (actor.can(Capability::PageEdit)
            && actor.user_id.is_some()
            && actor.user_id == file.uploader_id)
}

/// Adds a version to a file's history, inside the caller's transaction.
pub(crate) async fn add_version(
    conn: &mut sqlx::PgConnection,
    media_id: Uuid,
    stored: &Stored,
    filename: &str,
    uploader: Option<Uuid>,
    comment: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query!(
        "INSERT INTO media_versions (id, media_id, storage_key, filename, mime, size_bytes,
                                     width, height, uploader_id, comment)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        Uuid::new_v4(),
        media_id,
        stored.key,
        filename,
        stored.kind.mime,
        stored.size as i64,
        stored.width.map(|w| w as i32),
        stored.height.map(|h| h as i32),
        uploader,
        comment
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Whether a stored upload may be sent to anybody: false while every file
/// that has it, now or in its history, is hidden. Bytes nobody recorded are
/// served as before.
pub(crate) async fn servable(db: &sqlx::PgPool, key: &str) -> Result<bool, AppError> {
    let visible = sqlx::query_scalar!(
        "SELECT bool_or(m.hidden_at IS NULL) FROM media_versions v JOIN media m ON m.id = v.media_id
         WHERE v.storage_key = $1",
        key
    )
    .fetch_one(db)
    .await?;
    Ok(visible.unwrap_or(true))
}

/// Whether the reader may still see a hidden upload: a moderator of a wiki
/// that has it.
pub(crate) async fn staff_may_see(
    db: &sqlx::PgPool,
    ctx: &Ctx,
    key: &str,
) -> Result<bool, AppError> {
    if !ctx.actor.can(Capability::PageDelete) {
        return Ok(false);
    }
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM media_versions v JOIN media m ON m.id = v.media_id
                          WHERE v.storage_key = $1 AND m.wiki_id = $2) AS "present!""#,
        key,
        ctx.wiki.id
    )
    .fetch_one(db)
    .await?)
}

/// A file's versions, newest first, for its page.
pub(crate) async fn versions(
    db: &sqlx::PgPool,
    ctx: &Ctx,
    file: &Target,
    may_replace: bool,
) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT v.id, v.storage_key, v.filename, v.mime, v.size_bytes, v.width, v.height,
                  v.comment, v.created_at,
                  (SELECT u.username FROM users u WHERE u.id = v.uploader_id) AS "uploader?"
           FROM media_versions v WHERE v.media_id = $1
           ORDER BY v.created_at DESC, v.id DESC"#,
        file.id
    )
    .fetch_all(db)
    .await?;
    // The newest row is the file as it stands; an older one with the same
    // bytes is the same picture and has nothing to bring back.
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let current = i == 0;
            minijinja::context! {
                id => row.id.to_string(),
                url => crate::media::url_for_key(&row.storage_key),
                filename => row.filename,
                mime => row.mime,
                size => crate::files::human_size(ctx, row.size_bytes),
                width => row.width,
                height => row.height,
                comment => row.comment,
                at => ctx.day(row.created_at),
                uploader => row.uploader,
                current => current,
                may_restore => may_replace && !current && row.storage_key != file.storage_key,
            }
        })
        .collect())
}

/// A refusal on the file's own page, with the way back to it.
fn refuse(ctx: &Ctx, file_path: &str, status: StatusCode, key: &str) -> Result<Response, AppError> {
    pages::notice(
        ctx,
        status,
        &ctx.t("file.action_refused"),
        &ctx.t(key),
        &ctx.link(&format!("/{file_path}")),
        &ctx.t("file.back"),
    )
}

fn forbidden(ctx: &Ctx, file_path: &str) -> Result<Response, AppError> {
    refuse(ctx, file_path, StatusCode::FORBIDDEN, "file.not_allowed")
}

fn note(raw: &str) -> Option<String> {
    let text: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(NOTE_MAX)
        .collect();
    (!text.is_empty()).then_some(text)
}

async fn audit(
    state: &AppState,
    ctx: &Ctx,
    action: &'static str,
    file: &Target,
    meta: serde_json::Value,
) {
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action,
            entity_type: "media",
            entity_id: Some(file.id),
            meta,
        },
    )
    .await;
}

/// Makes `stored` the file as it stands and adds it to the history.
async fn make_current(
    db: &sqlx::PgPool,
    uploader: Option<Uuid>,
    file: &Target,
    stored: &Stored,
    filename: &str,
    comment: Option<&str>,
) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    sqlx::query!(
        "UPDATE media SET storage_key = $2, filename = $3, mime = $4, size_bytes = $5,
                          width = $6, height = $7
         WHERE id = $1",
        file.id,
        stored.key,
        filename,
        stored.kind.mime,
        stored.size as i64,
        stored.width.map(|w| w as i32),
        stored.height.map(|h| h as i32)
    )
    .execute(&mut *tx)
    .await?;
    add_version(&mut tx, file.id, stored, filename, uploader, comment).await?;
    tx.commit().await?;
    Ok(())
}

/// Bytes that are another file of this wiki: its name.
async fn taken_by(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    file: &Target,
    key: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar!(
        "SELECT name FROM media WHERE wiki_id = $1 AND storage_key = $2 AND id <> $3",
        wiki_id,
        key,
        file.id
    )
    .fetch_optional(db)
    .await?)
}

/// POST /{slug}/new-version
pub async fn new_version(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(file) = target(&state.db, ctx.wiki.id, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let path = file.path();
    if !may_replace(&ctx.actor, &file) {
        return forbidden(&ctx, &path);
    }
    if !crate::media::within_daily_quota(&state, &ctx).await? {
        return refuse_upload(&state, &ctx, &path, Refusal::Quota);
    }
    let max = state.config.upload_max_bytes;
    let mut upload = None;
    let mut comment = String::new();
    // The note may come before or after the file.
    while let Ok(Some(mut field)) = multipart.next_field().await {
        match field.name() {
            Some("file") => {
                let filename = field.file_name().unwrap_or("file").to_string();
                let mut data = Vec::new();
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) if data.len() + chunk.len() <= max => {
                            data.extend_from_slice(&chunk);
                        }
                        Ok(None) => break,
                        _ => return refuse_upload(&state, &ctx, &path, Refusal::TooLarge),
                    }
                }
                upload = Some((filename, data));
            }
            Some("comment") => {
                let text = field.text().await.unwrap_or_default();
                comment = text.chars().take(NOTE_MAX * 2).collect();
            }
            _ => {}
        }
    }
    let Some((filename, data)) = upload.filter(|(_, data)| !data.is_empty()) else {
        return refuse_upload(&state, &ctx, &path, Refusal::Empty);
    };
    // Another kind would need another prefix and another player.
    match crate::media::sniff(&data) {
        Some(kind) if kind.class == file.kind => {}
        Some(_) => {
            return refuse(
                &ctx,
                &path,
                StatusCode::UNPROCESSABLE_ENTITY,
                "file.other_kind",
            );
        }
        None => return refuse_upload(&state, &ctx, &path, Refusal::NotAnImage),
    }
    let stored = match crate::media::store(&state, "media", data, max).await? {
        Ok(stored) => stored,
        Err(refusal) => return refuse_upload(&state, &ctx, &path, refusal),
    };
    if stored.key == file.storage_key {
        return refuse(&ctx, &path, StatusCode::CONFLICT, "file.same_version");
    }
    if let Some(other) = taken_by(&state.db, ctx.wiki.id, &file, &stored.key).await? {
        return pages::notice(
            &ctx,
            StatusCode::CONFLICT,
            &ctx.t("file.action_refused"),
            &ctx.t_with("file.taken_by", &[("name", &other)]),
            &ctx.link(&format!("/{}:{other}", crate::files::prefix_of(&file.kind))),
            &ctx.t("file.open_other"),
        );
    }
    let filename = crate::media::clean_filename(&filename);
    let comment = note(&comment);
    make_current(
        &state.db,
        ctx.actor.user_id,
        &file,
        &stored,
        &filename,
        comment.as_deref(),
    )
    .await?;
    audit(
        &state,
        &ctx,
        "media.version",
        &file,
        json!({ "name": file.name, "was": file.storage_key, "now": stored.key, "size": stored.size, "mime": stored.kind.mime }),
    )
    .await;
    Ok(pages::see_other(&format!(
        "{}#file-versions",
        ctx.link(&format!("/{path}"))
    )))
}

fn refuse_upload(
    state: &AppState,
    ctx: &Ctx,
    path: &str,
    refusal: Refusal,
) -> Result<Response, AppError> {
    pages::notice(
        ctx,
        refusal.status(),
        &ctx.t("file.action_refused"),
        &crate::media::refusal_message(state, ctx, refusal),
        &ctx.link(&format!("/{path}")),
        &ctx.t("file.back"),
    )
}

#[derive(serde::Deserialize)]
pub struct RestoreForm {
    version: String,
}

/// POST /{slug}/restore-version
pub async fn restore(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<RestoreForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(file) = target(&state.db, ctx.wiki.id, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let path = file.path();
    if !may_replace(&ctx.actor, &file) {
        return forbidden(&ctx, &path);
    }
    // Only a version of this very file: the form field is not trusted.
    let Some(old) = (match pages::parse_uuid(&form.version) {
        Some(id) => {
            sqlx::query!(
                "SELECT storage_key, filename, mime, size_bytes, width, height, created_at
             FROM media_versions WHERE id = $1 AND media_id = $2",
                id,
                file.id
            )
            .fetch_optional(&state.db)
            .await?
        }
        None => None,
    }) else {
        return Ok(crate::errors::not_found());
    };
    if old.storage_key == file.storage_key {
        return refuse(&ctx, &path, StatusCode::CONFLICT, "file.same_version");
    }
    if let Some(other) = taken_by(&state.db, ctx.wiki.id, &file, &old.storage_key).await? {
        return pages::notice(
            &ctx,
            StatusCode::CONFLICT,
            &ctx.t("file.action_refused"),
            &ctx.t_with("file.taken_by", &[("name", &other)]),
            &ctx.link(&format!("/{}:{other}", crate::files::prefix_of(&file.kind))),
            &ctx.t("file.open_other"),
        );
    }
    let Some(kind) = crate::media::mime_for(&old.storage_key) else {
        return Err(AppError::Internal);
    };
    let stored = Stored {
        url: crate::media::url_for_key(&old.storage_key),
        key: old.storage_key,
        kind,
        width: old.width.map(|w| w as u32),
        height: old.height.map(|h| h as u32),
        size: old.size_bytes as usize,
    };
    let comment = ctx.t_with("file.restored_note", &[("when", &ctx.day(old.created_at))]);
    make_current(
        &state.db,
        ctx.actor.user_id,
        &file,
        &stored,
        &old.filename,
        Some(&comment),
    )
    .await?;
    audit(
        &state,
        &ctx,
        "media.restore",
        &file,
        json!({ "name": file.name, "was": file.storage_key, "now": stored.key }),
    )
    .await;
    Ok(pages::see_other(&format!(
        "{}#file-versions",
        ctx.link(&format!("/{path}"))
    )))
}

#[derive(serde::Deserialize)]
pub struct VisibilityForm {
    /// "hide" or "show".
    action: String,
    #[serde(default)]
    reason: String,
}

/// POST /{slug}/visibility
pub async fn visibility(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<VisibilityForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(file) = target(&state.db, ctx.wiki.id, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let path = file.path();
    if !ctx.actor.can(Capability::PageDelete) {
        return forbidden(&ctx, &path);
    }
    let hide = match form.action.as_str() {
        "hide" => true,
        "show" => false,
        _ => return Ok(crate::errors::not_found()),
    };
    let reason = note(&form.reason);
    if hide {
        sqlx::query!(
            "UPDATE media SET hidden_at = now(), hidden_by = $2, hidden_reason = $3 WHERE id = $1",
            file.id,
            ctx.actor.user_id,
            reason
        )
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query!(
            "UPDATE media SET hidden_at = NULL, hidden_by = NULL, hidden_reason = NULL WHERE id = $1",
            file.id
        )
        .execute(&state.db)
        .await?;
    }
    audit(
        &state,
        &ctx,
        if hide { "media.hide" } else { "media.show" },
        &file,
        json!({ "name": file.name, "reason": reason }),
    )
    .await;
    Ok(pages::see_other(&ctx.link(&format!("/{path}"))))
}

#[derive(serde::Deserialize)]
pub struct DeleteForm {
    /// The file's name, typed again.
    #[serde(default)]
    confirm: String,
}

/// POST /{slug}/delete-file
pub async fn delete(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(file) = target(&state.db, ctx.wiki.id, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    let path = file.path();
    if !ctx.actor.can(Capability::AdminPanel) {
        return forbidden(&ctx, &path);
    }
    if form.confirm.trim().to_lowercase() != file.name {
        return refuse(
            &ctx,
            &path,
            StatusCode::UNPROCESSABLE_ENTITY,
            "file.delete_confirm_wrong",
        );
    }
    let keys = remove(&state.db, file.id).await?;
    let mut removed = 0;
    for key in &keys {
        match state.storage.delete(key).await {
            Ok(()) => removed += 1,
            // The rows are gone; a stray object only takes space.
            Err(err) => tracing::warn!(%key, %err, "could not remove a deleted file's bytes"),
        }
    }
    audit(
        &state,
        &ctx,
        "media.delete",
        &file,
        json!({ "name": file.name, "key": file.storage_key, "objects_removed": removed }),
    )
    .await;
    pages::notice_ok(
        &ctx,
        &ctx.t("file.deleted_title"),
        &ctx.t_with("file.deleted_body", &[("name", &path)]),
        &ctx.link("/media"),
        &ctx.t("file.to_uploads"),
    )
}

/// Removes a file and its history, and returns the stored keys nothing else
/// has any more.
pub(crate) async fn remove(db: &sqlx::PgPool, media_id: Uuid) -> Result<Vec<String>, AppError> {
    let mut tx = db.begin().await?;
    let keys = sqlx::query_scalar!(
        "SELECT DISTINCT storage_key FROM media_versions WHERE media_id = $1",
        media_id
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM media WHERE id = $1", media_id)
        .execute(&mut *tx)
        .await?;
    // Any wiki's file may share the bytes: storage is keyed by content.
    let orphans = sqlx::query_scalar!(
        r#"SELECT k AS "k!" FROM unnest($1::text[]) AS k
           WHERE NOT EXISTS (SELECT 1 FROM media_versions v WHERE v.storage_key = k)
             AND NOT EXISTS (SELECT 1 FROM media m WHERE m.storage_key = k)"#,
        &keys
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(orphans)
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::perm::{GlobalRole, Rules, WikiRole};
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

    fn stored(hash_char: char) -> Stored {
        let hash: String = std::iter::repeat_n(hash_char, 64).collect();
        let key = format!("media/{}/{hash}.png", &hash[..2]);
        Stored {
            url: crate::media::url_for_key(&key),
            key,
            kind: crate::media::mime_for("x.png").expect("png"),
            width: Some(10),
            height: Some(10),
            size: 100,
        }
    }

    /// A file as an upload records it: the row and its first version.
    async fn upload(db: &PgPool, wiki_id: Uuid, name: &str, bytes: &Stored) -> Target {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO media (id, wiki_id, storage_key, filename, mime, size_bytes, name, kind)
             VALUES ($1, $2, $3, 'up.png', 'image/png', 100, $4, 'image')",
        )
        .bind(id)
        .bind(wiki_id)
        .bind(&bytes.key)
        .bind(name)
        .execute(db)
        .await
        .expect("media");
        let mut conn = db.acquire().await.expect("conn");
        add_version(&mut conn, id, bytes, "up.png", None, None)
            .await
            .expect("version");
        target(db, wiki_id, &format!("image:{name}"))
            .await
            .expect("target")
            .expect("found")
    }

    async fn hide(db: &PgPool, file: &Target, hidden: bool) {
        sqlx::query("UPDATE media SET hidden_at = CASE WHEN $2 THEN now() END WHERE id = $1")
            .bind(file.id)
            .bind(hidden)
            .execute(db)
            .await
            .expect("hide");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_new_version_replaces_the_file_everywhere_and_keeps_the_old(db: PgPool) {
        let here = wiki(&db, "here").await;
        let there = wiki(&db, "there").await;
        let (a, b) = (stored('a'), stored('b'));
        let file = upload(&db, here, "cat.png", &a).await;
        let twin = upload(&db, there, "cat.png", &a).await;

        make_current(&db, None, &file, &b, "better.png", Some("larger"))
            .await
            .expect("new version");

        let md = "![Cat](image:cat.png)".to_string();
        let now_here = crate::files::resolve(&db, here, md.clone())
            .await
            .expect("resolve");
        assert!(
            now_here.contains(&b.url),
            "the article shows the new version: {now_here}"
        );
        let now_there = crate::files::resolve(&db, there, md)
            .await
            .expect("resolve");
        assert!(
            now_there.contains(&a.url),
            "the other wiki's file of the same name is its own: {now_there}"
        );
        let history: Vec<String> = sqlx::query_scalar(
            "SELECT storage_key FROM media_versions WHERE media_id = $1 ORDER BY created_at, id",
        )
        .bind(file.id)
        .fetch_all(&db)
        .await
        .expect("history");
        assert_eq!(history.len(), 2, "both versions stay: {history:?}");
        assert!(history.contains(&a.key) && history.contains(&b.key));
        let twin_versions: i64 =
            sqlx::query_scalar("SELECT count(*) FROM media_versions WHERE media_id = $1")
                .bind(twin.id)
                .fetch_one(&db)
                .await
                .expect("count");
        assert_eq!(twin_versions, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_hidden_file_is_kept_from_readers_unless_another_wiki_shows_it(db: PgPool) {
        let here = wiki(&db, "here").await;
        let there = wiki(&db, "there").await;
        let a = stored('c');
        let file = upload(&db, here, "cat.png", &a).await;

        hide(&db, &file, true).await;
        assert!(!servable(&db, &a.key).await.expect("servable"));
        let md = crate::files::resolve(&db, here, "![Cat](image:cat.png)".into())
            .await
            .expect("resolve");
        assert!(!md.contains(&a.url), "articles no longer show it: {md}");
        assert!(md.contains("/image:cat.png"), "they link to its page: {md}");

        // The same bytes, visible in another wiki, are that wiki's to serve.
        let twin = upload(&db, there, "same.png", &a).await;
        assert!(servable(&db, &a.key).await.expect("servable"));
        hide(&db, &twin, true).await;
        assert!(!servable(&db, &a.key).await.expect("servable"));

        hide(&db, &file, false).await;
        assert!(servable(&db, &a.key).await.expect("servable"));
        // Bytes no file recorded are served as before.
        assert!(
            servable(&db, "media/ff/unknown.png")
                .await
                .expect("servable")
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deleting_a_file_frees_only_bytes_nobody_else_has(db: PgPool) {
        let here = wiki(&db, "here").await;
        let there = wiki(&db, "there").await;
        let (shared, own, old) = (stored('d'), stored('e'), stored('f'));
        let file = upload(&db, here, "cat.png", &old).await;
        make_current(&db, None, &file, &own, "own.png", None)
            .await
            .expect("v2");
        make_current(&db, None, &file, &shared, "shared.png", None)
            .await
            .expect("v3");
        upload(&db, there, "dog.png", &shared).await;

        let mut freed = remove(&db, file.id).await.expect("remove");
        freed.sort();
        let mut expected = vec![old.key.clone(), own.key.clone()];
        expected.sort();
        assert_eq!(freed, expected, "the other wiki keeps the shared bytes");
        assert!(
            target(&db, here, "image:cat.png")
                .await
                .expect("target")
                .is_none(),
            "the file is gone"
        );
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM media_versions v JOIN media m ON m.id = v.media_id
             WHERE m.wiki_id = $1",
        )
        .bind(here)
        .fetch_one(&db)
        .await
        .expect("count");
        assert_eq!(left, 0, "and so is its history");
    }

    fn actor(user_id: Uuid, membership: WikiRole) -> Actor {
        Actor {
            user_id: Some(user_id),
            username: Some("tester".into()),
            display_name: None,
            avatar_url: None,
            email_verified: true,
            global: GlobalRole::Registered,
            membership: Some(membership),
            rules: Rules::default(),
            overrides: Vec::new(),
            sanction: None,
        }
    }

    #[test]
    fn the_uploader_and_curators_replace_a_file_and_moderators_a_hidden_one() {
        let uploader = Uuid::new_v4();
        let mut file = Target {
            id: Uuid::new_v4(),
            name: "cat.png".into(),
            kind: "image".into(),
            storage_key: "media/aa/x.png".into(),
            uploader_id: Some(uploader),
            hidden: false,
        };
        let stranger = actor(Uuid::new_v4(), WikiRole::Registered);
        assert!(may_replace(&actor(uploader, WikiRole::Registered), &file));
        assert!(!may_replace(&stranger, &file), "not somebody else's file");
        assert!(may_replace(
            &actor(Uuid::new_v4(), WikiRole::Curator),
            &file
        ));
        assert!(!may_replace(&Actor::anonymous(Rules::default()), &file));

        file.hidden = true;
        assert!(!may_replace(&actor(uploader, WikiRole::Registered), &file));
        assert!(!may_replace(
            &actor(Uuid::new_v4(), WikiRole::Curator),
            &file
        ));
        assert!(may_replace(
            &actor(Uuid::new_v4(), WikiRole::Moderator),
            &file
        ));
    }
}
