//! The wiki's documents (its terms and privacy policy), what people agreed
//! to, and deleting an account.
//!
//! Documents. The pages themselves are ordinary pages, `/terms` and
//! `/privacy`, edited like any other. What this module adds is a version in
//! `wikis.settings.legal`, which an admin raises when a document changes in
//! substance, and each account's `policy_version`, the last one it accepted.
//! With `consent` on, signing in takes a ticked box, which records the current
//! version; with `notify` on, a reader who has not seen the latest version is
//! told once, on their next visit. An admin may also raise the version without
//! telling anyone, which the privacy policy reserves.
//!
//! Deleting. An account can go in three ways, from the least to the most:
//! - `Account`: the account and profile go; edits and files stay, under a
//!   `deleted-...` name. What a person can do themselves, when allowed.
//! - `Files`: as above, and every file they uploaded is hidden.
//! - `Full`: as above, and wherever their edit is the current text it is
//!   rolled back to the last version by someone else; a page only they wrote
//!   is archived. An admin's exception, never the default.
//!
//! The account's row stays, so history keeps an author, but it loses its
//! name, profile, contacts and every way to sign in.

use axum::extract::{Extension, Form, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::redirect::safe_next;
use crate::auth::session::CurrentUser;
use crate::pages;
use crate::resolve::Ctx;

/// Remembers, for a reader without an account, the version they were told about.
pub const COOKIE: &str = "naw_policy";

/// How long after a change a reader without an account is told about it. A
/// first visit long after the change is no change for them.
const ANONYMOUS_NOTICE_DAYS: i64 = 30;

/// `wikis.settings.legal`, with the defaults of a wiki that never set it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Raised by an admin on a change in substance. Starts at 1.
    pub version: i32,
    /// Tell readers about the latest version.
    pub notify: bool,
    /// When the version was last raised.
    pub changed_at: Option<DateTime<Utc>>,
    /// Signing in takes a ticked box.
    pub consent: bool,
    /// People may delete their own account from the settings.
    pub self_delete: bool,
}

impl Policy {
    pub fn of(settings: &Value) -> Self {
        let block = settings.get("legal");
        let field = |key: &str| block.and_then(|b| b.get(key));
        let flag =
            |key: &str, default: bool| field(key).and_then(Value::as_bool).unwrap_or(default);
        Self {
            version: field("version")
                .and_then(Value::as_i64)
                .unwrap_or(1)
                .clamp(1, i64::from(i32::MAX)) as i32,
            notify: flag("notify", false),
            changed_at: field("changed_at")
                .and_then(Value::as_str)
                .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                .map(|at| at.with_timezone(&Utc)),
            consent: flag("consent", true),
            self_delete: flag("self_delete", true),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "version": self.version,
            "notify": self.notify,
            "changed_at": self.changed_at.map(|at| at.to_rfc3339()),
            "consent": self.consent,
            "self_delete": self.self_delete,
        })
    }
}

/// The version in the reader's cookie, 0 without one.
pub fn cookie_version(headers: &HeaderMap) -> i32 {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|line| line.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Whether to tell this reader about the latest version: an account by what
/// it accepted, anyone else by their cookie, and only while the change is new.
pub fn notice_due(policy: &Policy, accepted: Option<i32>, cookie: i32, now: DateTime<Utc>) -> bool {
    if !policy.notify {
        return false;
    }
    match accepted {
        Some(version) => version < policy.version,
        None => {
            cookie < policy.version
                && policy
                    .changed_at
                    .is_some_and(|at| now - at < Duration::days(ANONYMOUS_NOTICE_DAYS))
        }
    }
}

/// Records that `user_id` accepted `version`. Never lowers it.
pub async fn accept(db: &sqlx::PgPool, user_id: Uuid, version: i32) -> Result<(), AppError> {
    sqlx::query!(
        "UPDATE users SET policy_version = GREATEST(policy_version, $2), policy_accepted_at = now()
         WHERE id = $1",
        user_id,
        version
    )
    .execute(db)
    .await?;
    Ok(())
}

/// The cookie that remembers the version for a reader without an account.
fn cookie_for(version: i32) -> String {
    cookie::Cookie::build((COOKIE, version.to_string()))
        .path("/")
        .http_only(true)
        .same_site(cookie::SameSite::Lax)
        .max_age(cookie::time::Duration::days(365))
        .build()
        .to_string()
}

#[derive(Deserialize, Default)]
pub struct AckForm {
    #[serde(default)]
    next: String,
}

/// POST /legal/ack: "got it" on the notice. An account accepts the version.
pub async fn ack(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<AckForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let policy = Policy::of(&ctx.wiki.settings);
    if let Some(user) = &user {
        accept(&state.db, user.id, policy.version).await?;
    }
    let mut response = pages::see_other(&safe_next(Some(&form.next)));
    if let Ok(value) = HeaderValue::from_str(&cookie_for(policy.version)) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    Ok(response)
}

// ---------------------------------------------------------------------------
// Admin, Documents
// ---------------------------------------------------------------------------

async fn save(state: &AppState, ctx: &Ctx, policy: &Policy) -> Result<(), AppError> {
    let mut settings = ctx.wiki.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    if let Some(object) = settings.as_object_mut() {
        object.insert("legal".to_string(), policy.to_json());
    }
    sqlx::query!(
        "UPDATE wikis SET settings = $2 WHERE id = $1",
        ctx.wiki.id,
        settings
    )
    .execute(&state.db)
    .await?;
    Ok(())
}

#[derive(Deserialize, Default)]
pub struct DoneQuery {
    #[serde(default)]
    done: Option<String>,
}

/// GET /admin/legal
pub async fn admin_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<DoneQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::admin::gate(&state, &headers, user.as_ref()).await);
    let policy = Policy::of(&ctx.wiki.settings);
    let counts = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE policy_version >= $1) AS "accepted!",
                  count(*) AS "accounts!"
           FROM users WHERE deleted_at IS NULL"#,
        policy.version
    )
    .fetch_one(&state.db)
    .await?;
    crate::admin::render(
        &ctx,
        "legal",
        &ctx.t("legal.admin_title"),
        minijinja::context! {
            policy_version => policy.version,
            policy_notify => policy.notify,
            policy_consent => policy.consent,
            policy_self_delete => policy.self_delete,
            policy_changed => policy.changed_at.map(|at| ctx.day(at)),
            accepted => counts.accepted,
            accounts => counts.accounts,
            terms_href => ctx.link("/terms"),
            privacy_href => ctx.link("/privacy"),
            done => pages::message_key(query.done.as_deref(), &[""]),
            is_owner => ctx.actor.effective_role() == Some(crate::perm::WikiRole::Owner),
            owners_only => ctx.wiki.settings.get("protected_pages").and_then(Value::as_str) == Some("owner"),
        },
    )
}

#[derive(Deserialize, Default)]
pub struct SettingsForm {
    #[serde(default)]
    consent: Option<String>,
    #[serde(default)]
    self_delete: Option<String>,
}

/// POST /admin/legal: what signing in and deleting ask for.
pub async fn admin_save(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::admin::gate(&state, &headers, user.as_ref()).await);
    let mut policy = Policy::of(&ctx.wiki.settings);
    policy.consent = form.consent.is_some();
    policy.self_delete = form.self_delete.is_some();
    save(&state, &ctx, &policy).await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "wiki.legal_settings",
            entity_type: "wiki",
            entity_id: Some(ctx.wiki.id),
            meta: json!({ "consent": policy.consent, "self_delete": policy.self_delete }),
        },
    )
    .await;
    Ok(pages::see_other("/admin/legal?done=saved"))
}

#[derive(Deserialize, Default)]
pub struct AnnounceForm {
    #[serde(default)]
    notify: Option<String>,
}

/// POST /admin/legal/announce: a new version of the documents, told to
/// readers or not.
pub async fn admin_announce(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<AnnounceForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::admin::gate(&state, &headers, user.as_ref()).await);
    let mut policy = Policy::of(&ctx.wiki.settings);
    policy.version = policy.version.saturating_add(1);
    policy.notify = form.notify.is_some();
    policy.changed_at = Some(Utc::now());
    save(&state, &ctx, &policy).await?;
    // The admin announcing it has read it.
    if let Some(id) = ctx.actor.user_id {
        accept(&state.db, id, policy.version).await?;
    }
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "wiki.legal_version",
            entity_type: "wiki",
            entity_id: Some(ctx.wiki.id),
            meta: json!({ "version": policy.version, "notify": policy.notify }),
        },
    )
    .await;
    Ok(pages::see_other("/admin/legal?done=announced"))
}

#[derive(Deserialize, Default)]
pub struct ProtectForm {
    #[serde(default)]
    level: String,
}

/// POST /admin/legal/protect: who edits the wiki's own pages (see
/// `pages::system_floor`), admins or owners only. An owner's choice, so an
/// admin can neither make it nor undo it.
pub async fn admin_protect(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<ProtectForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::admin::gate(&state, &headers, user.as_ref()).await);
    if ctx.actor.effective_role() != Some(crate::perm::WikiRole::Owner) {
        return Ok(pages::see_other("/admin/legal?done=owner_only"));
    }
    let level = if form.level == "owner" {
        "owner"
    } else {
        "admin"
    };
    let mut settings = ctx.wiki.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    if let Some(object) = settings.as_object_mut() {
        object.insert("protected_pages".to_string(), json!(level));
    }
    sqlx::query!(
        "UPDATE wikis SET settings = $2 WHERE id = $1",
        ctx.wiki.id,
        settings
    )
    .execute(&state.db)
    .await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "wiki.system_pages",
            entity_type: "wiki",
            entity_id: Some(ctx.wiki.id),
            meta: json!({ "level": level }),
        },
    )
    .await;
    Ok(pages::see_other("/admin/legal?done=protected"))
}

// ---------------------------------------------------------------------------
// Deleting an account
// ---------------------------------------------------------------------------

/// How much goes with an account; see the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Erase {
    Account,
    Files,
    Full,
}

impl Erase {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "account" => Some(Self::Account),
            "files" => Some(Self::Files),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Files => "files",
            Self::Full => "full",
        }
    }
}

/// The name a deleted account gets: unique, and never claimable (see
/// `username::is_deleted_name`).
fn deleted_name(user_id: Uuid) -> String {
    format!(
        "{}{}",
        crate::auth::username::DELETED_PREFIX,
        &user_id.simple().to_string()[..12]
    )
}

/// Deletes the account `user_id` on behalf of `ctx.actor`: the account and
/// profile always, files and current text as `erase` says. Returns the name
/// it now goes by. Files and text are dealt with on `ctx.wiki` only.
pub async fn delete_account(
    state: &AppState,
    ctx: &mut Ctx,
    user_id: Uuid,
    erase: Erase,
) -> Result<Option<String>, AppError> {
    let Some(before) = sqlx::query_scalar!(
        "SELECT username FROM users WHERE id = $1 AND deleted_at IS NULL",
        user_id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(None);
    };
    let name = deleted_name(user_id);
    let by = ctx.actor.user_id;
    let mut tx = state.db.begin().await?;
    // The profile pages, under the name it had and every old one.
    sqlx::query!(
        "UPDATE pages SET deleted_at = now(), deleted_by = $3
         WHERE namespace = 'user' AND deleted_at IS NULL
           AND (slug = $1 OR slug IN (SELECT alias FROM user_aliases WHERE user_id = $2))",
        before,
        user_id,
        by
    )
    .execute(&mut *tx)
    .await?;
    // The bots' chats and their language choices.
    sqlx::query!(
        "DELETE FROM bot_chat_languages b USING telegram_links t
         WHERE t.user_id = $1 AND b.platform = 'telegram' AND b.chat_id = t.chat_id::text",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM bot_chat_languages b USING discord_links d
         WHERE d.user_id = $1 AND b.platform = 'discord' AND b.chat_id = d.discord_id",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    // Every way to sign in, every contact, and what was only theirs.
    for statement in [
        "DELETE FROM sessions WHERE user_id = $1",
        "DELETE FROM oauth_identities WHERE user_id = $1",
        "DELETE FROM access_tokens WHERE user_id = $1",
        "DELETE FROM trusted_devices WHERE user_id = $1",
        "DELETE FROM webauthn_credentials WHERE user_id = $1",
        "DELETE FROM totp_credentials WHERE user_id = $1",
        "DELETE FROM recovery_codes WHERE user_id = $1",
        "DELETE FROM password_resets WHERE user_id = $1",
        "DELETE FROM email_verifications WHERE user_id = $1",
        "DELETE FROM telegram_links WHERE user_id = $1",
        "DELETE FROM discord_links WHERE user_id = $1",
        "DELETE FROM drafts WHERE user_id = $1",
        "DELETE FROM watches WHERE user_id = $1",
        "DELETE FROM watchlist WHERE user_id = $1",
        "DELETE FROM notifications WHERE user_id = $1",
        "DELETE FROM notification_log WHERE user_id = $1",
        "DELETE FROM user_aliases WHERE user_id = $1",
        "DELETE FROM wiki_memberships WHERE user_id = $1",
        "DELETE FROM user_capabilities WHERE user_id = $1",
        "DELETE FROM curatorships WHERE user_id = $1",
    ] {
        sqlx::query(statement)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query!(
        "UPDATE users SET username = $2, display_name = NULL, avatar_key = NULL,
                email = $2 || '@deleted.invalid', email_verified_at = NULL,
                password_hash = NULL, must_change_password = false, settings = '{}',
                global_role = 'registered', deleted_at = now()
         WHERE id = $1",
        user_id,
        name
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut files = 0;
    if matches!(erase, Erase::Files | Erase::Full) {
        files = sqlx::query!(
            "UPDATE media SET hidden_at = now(), hidden_by = $3, hidden_reason = 'account deleted'
             WHERE wiki_id = $1 AND uploader_id = $2 AND hidden_at IS NULL",
            ctx.wiki.id,
            user_id,
            by
        )
        .execute(&state.db)
        .await?
        .rows_affected();
    }
    let (mut rolled_back, mut archived) = (0, 0);
    if erase == Erase::Full {
        (rolled_back, archived) = take_back_text(state, ctx, user_id, &name).await?;
    }
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: by,
            action: "user.delete",
            entity_type: "user",
            entity_id: Some(user_id),
            meta: json!({
                "erase": erase.as_str(),
                "name": name,
                "files_hidden": files,
                "pages_rolled_back": rolled_back,
                "pages_archived": archived,
            }),
        },
    )
    .await;
    tracing::info!(%user_id, erase = erase.as_str(), "account deleted");
    Ok(Some(name))
}

/// Wherever the deleted account's edit is the current text of an article,
/// the last version by someone else comes back; an article only they wrote
/// is archived. Returns how many of each.
async fn take_back_text(
    state: &AppState,
    ctx: &mut Ctx,
    user_id: Uuid,
    name: &str,
) -> Result<(u32, u32), AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.id, p.slug, COALESCE(p.locale, '') AS "locale!"
           FROM pages p JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
             AND r.author_id = $2"#,
        ctx.wiki.id,
        user_id
    )
    .fetch_all(&state.db)
    .await?;
    let summary = ctx.t_with("legal.erase_summary", &[("who", name)]);
    let (mut rolled_back, mut archived) = (0, 0);
    for row in rows {
        let other = sqlx::query!(
            "SELECT id, body_md FROM revisions
             WHERE page_id = $1 AND review_status = 'accepted' AND author_id IS DISTINCT FROM $2
             ORDER BY created_at DESC, id DESC LIMIT 1",
            row.id,
            user_id
        )
        .fetch_optional(&state.db)
        .await?;
        match other {
            Some(target) => {
                let Some(found) =
                    pages::find_page(&state.db, ctx.wiki.id, &row.slug, &row.locale).await?
                else {
                    continue;
                };
                ctx.content_locale = if row.locale.is_empty() {
                    ctx.wiki.default_locale.clone()
                } else {
                    row.locale.clone()
                };
                let conflict = crate::history::restore(
                    state,
                    ctx,
                    &row.slug,
                    &found,
                    target.id,
                    &target.body_md,
                    &summary,
                    "page.rollback",
                )
                .await?;
                if conflict.is_none() {
                    rolled_back += 1;
                }
            }
            None => {
                sqlx::query!(
                    "UPDATE pages SET deleted_at = now(), deleted_by = $2
                     WHERE id = $1 AND deleted_at IS NULL",
                    row.id,
                    ctx.actor.user_id
                )
                .execute(&state.db)
                .await?;
                archived += 1;
            }
        }
    }
    Ok((rolled_back, archived))
}

#[derive(Deserialize, Default)]
pub struct SelfDeleteForm {
    #[serde(default)]
    confirm: String,
}

/// POST /settings/delete: a person deletes their own account, when the wiki
/// allows it. Only the account and profile; files and edits stay.
pub async fn self_delete(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<SelfDeleteForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(pages::see_other("/login?next=/settings"));
    };
    let mut ctx = or_respond!(crate::resolve::required(&state, &headers, Some(&user)).await?);
    if !Policy::of(&ctx.wiki.settings).self_delete {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    if !form.confirm.trim().eq_ignore_ascii_case(&user.username) {
        return Ok(pages::see_other("/settings?error=delete_mismatch#delete"));
    }
    delete_account(&state, &mut ctx, user.id, Erase::Account).await?;
    let secure = crate::auth::routes::secure_cookies(&state);
    let mut response = pages::see_other("/?deleted=1");
    if let Ok(value) = HeaderValue::from_str(&crate::auth::session::clear_cookie(secure)) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    Ok(response)
}

#[derive(Deserialize, Default)]
pub struct AdminDeleteForm {
    #[serde(default)]
    erase: String,
    #[serde(default)]
    confirm: String,
}

/// POST /admin/user/{name}/delete: an admin deletes an account, as far as
/// `erase` says. The operator's accounts and the admin's own are refused.
pub async fn admin_delete(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(form): Form<AdminDeleteForm>,
) -> Result<Response, AppError> {
    let mut ctx = or_respond!(crate::admin::gate(&state, &headers, user.as_ref()).await);
    let name = name.trim().to_lowercase();
    let back = format!("/admin/user/{name}");
    let Some(target) = sqlx::query!(
        "SELECT id, global_role FROM users WHERE lower(username) = $1 AND deleted_at IS NULL",
        name
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    let Some(erase) = Erase::parse(&form.erase) else {
        return Ok(pages::see_other(&format!("{back}?done=delete_choose")));
    };
    if !form.confirm.trim().eq_ignore_ascii_case(&name) {
        return Ok(pages::see_other(&format!("{back}?done=delete_mismatch")));
    }
    if target.global_role == "root" || Some(target.id) == ctx.actor.user_id {
        return Ok(pages::see_other(&format!("{back}?done=delete_refused")));
    }
    let Some(new_name) = delete_account(&state, &mut ctx, target.id, erase).await? else {
        return Ok(crate::errors::not_found());
    };
    Ok(pages::see_other(&format!(
        "/admin/user/{new_name}?done=deleted"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn a_wiki_that_never_set_its_documents_asks_for_consent_and_tells_no_one() {
        let policy = Policy::of(&json!({}));
        assert_eq!(policy.version, 1);
        assert!(policy.consent);
        assert!(policy.self_delete);
        assert!(!policy.notify);
        let policy = Policy::of(&json!({ "legal": { "version": -4, "consent": "yes" } }));
        assert_eq!(policy.version, 1, "a nonsense version reads as the first");
        assert!(policy.consent, "a malformed switch keeps its default");
    }

    #[test]
    fn the_settings_round_trip() {
        let policy = Policy {
            version: 3,
            notify: true,
            changed_at: Some(
                DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            consent: false,
            self_delete: false,
        };
        assert_eq!(Policy::of(&json!({ "legal": policy.to_json() })), policy);
    }

    #[test]
    fn an_account_is_told_until_it_accepts_and_a_guest_while_the_change_is_new() {
        let mut policy = Policy::of(&json!({}));
        policy.version = 2;
        policy.notify = true;
        policy.changed_at = Some(now() - Duration::days(2));
        assert!(
            notice_due(&policy, Some(1), 0, now()),
            "an account on the old version"
        );
        assert!(
            !notice_due(&policy, Some(2), 0, now()),
            "an account that accepted"
        );
        assert!(
            notice_due(&policy, None, 1, now()),
            "a guest who saw the old one"
        );
        assert!(
            !notice_due(&policy, None, 2, now()),
            "a guest who saw this one"
        );
        policy.changed_at = Some(now() - Duration::days(ANONYMOUS_NOTICE_DAYS + 1));
        assert!(
            !notice_due(&policy, None, 0, now()),
            "an old change is no news to a guest"
        );
        assert!(
            notice_due(&policy, Some(1), 0, now()),
            "an account is still asked"
        );
        policy.notify = false;
        assert!(
            !notice_due(&policy, Some(1), 0, now()),
            "a quiet change tells no one"
        );
    }

    #[test]
    fn the_cookie_is_read_among_others() {
        let mut headers = HeaderMap::new();
        assert_eq!(cookie_version(&headers), 0);
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("naw_lang=ru; naw_policy=3"),
        );
        assert_eq!(cookie_version(&headers), 3);
        headers.insert(header::COOKIE, HeaderValue::from_static("naw_policy=lots"));
        assert_eq!(cookie_version(&headers), 0);
        assert!(cookie_for(4).starts_with("naw_policy=4"));
    }

    #[test]
    fn a_deleted_name_cannot_be_claimed() {
        let name = deleted_name(Uuid::new_v4());
        assert!(crate::auth::username::is_deleted_name(&name));
        assert!(crate::auth::username::is_valid(&name), "{name}");
        assert_eq!(Erase::parse("files"), Some(Erase::Files));
        assert_eq!(Erase::parse("everything"), None);
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::PgPool;

    #[sqlx::test(migrations = "../../migrations")]
    async fn accepting_never_lowers_the_version(db: PgPool) {
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, username, email, password_hash) VALUES ($1, 'ann', 'ann@example.test', 'x')")
            .bind(user)
            .execute(&db)
            .await
            .expect("user");
        accept(&db, user, 3).await.expect("accept 3");
        accept(&db, user, 2).await.expect("accept 2");
        let version: i32 = sqlx::query_scalar("SELECT policy_version FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&db)
            .await
            .expect("read");
        assert_eq!(version, 3);
    }
}
