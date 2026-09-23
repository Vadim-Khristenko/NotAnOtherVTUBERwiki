//! One account, seen by the admins of this wiki: `/admin/user/{username}`.
//!
//! Everything about the person in one place: who they are, what they did here,
//! what they may do (role plus individual overrides), what was done about them
//! (sanctions, including lifted ones), and notes only admins read. Every
//! control is a POST, like the rest of the panel.
//!
//! Nobody acts on themselves here, and nobody acts on an account at or above
//! their own role: the same rule `admin::may_reset` applies to passwords.

use axum::extract::{Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::admin::{gate, may_reset, render};
use crate::audit;
use crate::auth::session::CurrentUser;
use crate::pages;
use crate::perm::{Actor, Capability, GlobalRole, Rules, WikiRole};
use crate::resolve::Ctx;

const REASON_MAX: usize = 500;
const NOTE_MAX: usize = 2000;
const RECENT: i64 = 6;

struct Target {
    id: Uuid,
    username: String,
    global: GlobalRole,
    role: Option<WikiRole>,
}

async fn target(state: &AppState, ctx: &Ctx, name: &str) -> Result<Option<Target>, AppError> {
    let row = sqlx::query!(
        r#"SELECT u.id, u.username, u.global_role, m.role::text AS "role?"
           FROM users u
           LEFT JOIN wiki_memberships m ON m.user_id = u.id AND m.wiki_id = $2
           WHERE lower(u.username) = $1"#,
        name.trim().to_lowercase(),
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|row| Target {
        id: row.id,
        username: row.username,
        global: GlobalRole::parse(&row.global_role),
        role: row.role.as_deref().and_then(WikiRole::parse),
    }))
}

fn back(t: &Target, done: &str) -> Response {
    pages::see_other(&format!("/admin/user/{}?done={done}", t.username))
}

fn refuse(reason: &'static str) -> Response {
    (StatusCode::FORBIDDEN, reason).into_response()
}

#[derive(serde::Deserialize, Default)]
pub struct DoneQuery {
    #[serde(default)]
    done: Option<String>,
}

/// GET /admin/user/{name}
pub async fn show(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(query): Query<DoneQuery>,
) -> Result<Response, AppError> {
    let ctx = match gate(&state, &headers, user.as_ref()).await {
        Ok(ctx) => ctx,
        Err(response) => return Ok(response),
    };
    let Some(t) = target(&state, &ctx, &name).await? else {
        return Ok(crate::errors::not_found());
    };

    let account = sqlx::query!(
        r#"SELECT u.display_name, u.email, u.email_verified_at, u.created_at,
                  u.must_change_password, (u.password_hash IS NOT NULL) AS "has_password!",
                  c.username AS "created_by?"
           FROM users u LEFT JOIN users c ON c.id = u.created_by
           WHERE u.id = $1"#,
        t.id
    )
    .fetch_one(&state.db)
    .await?;
    let stats = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id
                WHERE r.author_id = $1 AND p.wiki_id = $2) AS "edits!",
             (SELECT count(*) FROM pages p
                WHERE p.wiki_id = $2 AND (SELECT r.author_id FROM revisions r
                  WHERE r.page_id = p.id ORDER BY r.created_at LIMIT 1) = $1) AS "created!",
             (SELECT max(r.created_at) FROM revisions r JOIN pages p ON p.id = r.page_id
                WHERE r.author_id = $1 AND p.wiki_id = $2) AS last_edit,
             (SELECT max(created_at) FROM sessions WHERE user_id = $1) AS last_sign_in,
             (SELECT count(*) FROM sessions WHERE user_id = $1 AND expires_at > now()) AS "sessions!""#,
        t.id,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?;
    let identities: Vec<String> = sqlx::query_scalar!(
        "SELECT provider FROM oauth_identities WHERE user_id = $1 ORDER BY created_at",
        t.id
    )
    .fetch_all(&state.db)
    .await?;
    let aliases: Vec<String> = sqlx::query_scalar!(
        "SELECT alias FROM user_aliases WHERE user_id = $1 ORDER BY created_at DESC",
        t.id
    )
    .fetch_all(&state.db)
    .await?;

    // What the role alone gives this person, and what is overridden.
    let overrides = sqlx::query!(
        r#"SELECT o.capability, o.allowed, u.username AS "set_by?"
           FROM user_capabilities o LEFT JOIN users u ON u.id = o.set_by
           WHERE o.user_id = $1 AND o.wiki_id = $2"#,
        t.id,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let as_role = Actor {
        user_id: Some(t.id),
        username: Some(t.username.clone()),
        display_name: None,
        email_verified: account.email_verified_at.is_some(),
        global: t.global,
        membership: t.role,
        rules: Rules::from_settings(&ctx.wiki.settings),
        overrides: Vec::new(),
        sanction: None,
    };
    let manage_rights = ctx.actor.can(Capability::UserRoleManage);
    let capabilities: Vec<minijinja::Value> = Capability::ALL
        .iter()
        .map(|cap| {
            let set = overrides.iter().find(|o| o.capability == cap.as_str());
            minijinja::context! {
                id => cap.as_str(),
                label => ctx.t(&format!("admin.cap_{}", cap.as_str().replace('.', "_"))),
                by_role => as_role.can_by_role(*cap),
                state => match set.map(|o| o.allowed) { Some(true) => "allow", Some(false) => "deny", None => "inherit" },
                set_by => set.and_then(|o| o.set_by.clone()),
                editable => manage_rights && may_override(&ctx, *cap, true).is_ok(),
            }
        })
        .collect();

    let sanctions = sqlx::query!(
        r#"SELECT s.id, s.kind, s.reason, s.wiki_id, s.created_at, s.expires_at,
                  s.lifted_at, s.lift_reason,
                  c.username AS "by?", l.username AS "lifted_by?",
                  (s.lifted_at IS NULL AND (s.expires_at IS NULL OR s.expires_at > now())) AS "active!"
           FROM sanctions s
           LEFT JOIN users c ON c.id = s.created_by
           LEFT JOIN users l ON l.id = s.lifted_by
           WHERE s.user_id = $1 AND (s.wiki_id = $2 OR s.wiki_id IS NULL)
           ORDER BY s.created_at DESC"#,
        t.id,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let fmt = |t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%d %H:%M UTC").to_string();
    let sanctions: Vec<minijinja::Value> = sanctions
        .into_iter()
        .map(|s| {
            minijinja::context! {
                id => s.id.to_string(),
                kind => if s.wiki_id.is_none() { "ban_install".to_string() } else { s.kind.clone() },
                reason => s.reason,
                by => s.by,
                at => fmt(s.created_at),
                until => s.expires_at.map(fmt),
                active => s.active,
                lifted_at => s.lifted_at.map(fmt),
                lifted_by => s.lifted_by,
                lift_reason => s.lift_reason,
                install => s.wiki_id.is_none(),
            }
        })
        .collect();

    let notes = sqlx::query!(
        r#"SELECT n.body, n.created_at, a.username AS "author?"
           FROM user_notes n LEFT JOIN users a ON a.id = n.author_id
           WHERE n.user_id = $1 AND n.wiki_id = $2 ORDER BY n.created_at DESC"#,
        t.id,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let notes: Vec<minijinja::Value> = notes
        .into_iter()
        .map(
            |n| minijinja::context! { body => n.body, at => fmt(n.created_at), author => n.author },
        )
        .collect();

    let recent = sqlx::query!(
        r#"SELECT p.slug, p.title, COALESCE(p.locale, '') AS "locale!", r.created_at
           FROM revisions r JOIN pages p ON p.id = r.page_id
           WHERE r.author_id = $1 AND p.wiki_id = $2 AND p.namespace = 'main'
           ORDER BY r.created_at DESC LIMIT $3"#,
        t.id,
        ctx.wiki.id,
        RECENT
    )
    .fetch_all(&state.db)
    .await?;
    let recent: Vec<minijinja::Value> = recent
        .into_iter()
        .map(|r| {
            minijinja::context! {
                title => r.title,
                href => ctx.link_for(&r.locale, &format!("/{}", r.slug)),
                at => fmt(r.created_at),
            }
        })
        .collect();

    // The curator looking after this person, and who could.
    let curator = sqlx::query_scalar!(
        "SELECT u.username FROM curatorships c JOIN users u ON u.id = c.curator_id
         WHERE c.wiki_id = $1 AND c.user_id = $2",
        ctx.wiki.id,
        t.id
    )
    .fetch_optional(&state.db)
    .await?;
    let curator_choices: Vec<String> = sqlx::query_scalar!(
        r#"SELECT u.username FROM wiki_memberships m JOIN users u ON u.id = m.user_id
           WHERE m.wiki_id = $1 AND m.user_id <> $2
             AND m.role IN ('curator', 'moderator', 'admin', 'owner')
           ORDER BY u.username"#,
        ctx.wiki.id,
        t.id
    )
    .fetch_all(&state.db)
    .await?;

    let manageable = may_reset(&ctx, t.id, t.global, t.role);
    let grantable: Vec<&str> = WikiRole::ALL
        .iter()
        .filter(|role| ctx.actor.may_grant(**role))
        .map(|role| role.as_str())
        .collect();
    let fmt_opt = |t: Option<chrono::DateTime<chrono::Utc>>| t.map(fmt);
    render(
        &ctx,
        "user",
        &t.username,
        minijinja::context! {
            u_id => t.id.to_string(),
            u_name => t.username.clone(),
            u_display => account.display_name,
            u_email => account.email,
            u_email_verified => account.email_verified_at.is_some(),
            u_global => t.global.as_str(),
            u_role => t.role.map(WikiRole::as_str),
            u_joined => fmt(account.created_at),
            u_created_by => account.created_by,
            u_temporary => account.must_change_password,
            u_has_password => account.has_password,
            u_identities => identities,
            u_aliases => aliases,
            s_edits => stats.edits,
            s_created => stats.created,
            s_last_edit => fmt_opt(stats.last_edit),
            s_last_sign_in => fmt_opt(stats.last_sign_in),
            s_sessions => stats.sessions,
            capabilities => capabilities,
            sanctions => sanctions,
            notes => notes,
            recent => recent,
            is_me => ctx.actor.user_id == Some(t.id),
            manageable => manageable.is_ok(),
            manage_block => manageable.err(),
            can_install_ban => matches!(ctx.actor.global, GlobalRole::Root | GlobalRole::Staff),
            can_manage_rights => manage_rights,
            grantable => grantable,
            curator => curator,
            curator_choices => curator_choices,
            done => query.done.filter(|d| d.len() < 24 && d.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')),
        },
    )
}

/// Who may set an override for `cap`. Granting needs the capability yourself;
/// the ones that decide who may do what need an owner.
fn may_override(ctx: &Ctx, cap: Capability, granting: bool) -> Result<(), &'static str> {
    if !ctx.actor.can(Capability::UserRoleManage) {
        return Err("changing rights needs admin rights");
    }
    let owner =
        ctx.actor.global == GlobalRole::Root || ctx.actor.effective_role() == Some(WikiRole::Owner);
    if cap.owner_only() && !owner {
        return Err("only an owner may hand out this one");
    }
    if granting && !ctx.actor.can(cap) {
        return Err("you cannot grant what you do not have");
    }
    Ok(())
}

async fn resolve_managed(
    state: &AppState,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
    name: &str,
) -> Result<Result<(Ctx, Target), Response>, AppError> {
    let ctx = match gate(state, headers, user).await {
        Ok(ctx) => ctx,
        Err(response) => return Ok(Err(response)),
    };
    let Some(t) = target(state, &ctx, name).await? else {
        return Ok(Err(crate::errors::not_found()));
    };
    if let Err(reason) = may_reset(&ctx, t.id, t.global, t.role) {
        return Ok(Err(refuse(reason)));
    }
    Ok(Ok((ctx, t)))
}

#[derive(serde::Deserialize)]
pub struct CapabilityForm {
    capability: String,
    /// "allow", "deny" or "inherit".
    value: String,
}

/// POST /admin/user/{name}/capability
pub async fn set_capability(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(form): Form<CapabilityForm>,
) -> Result<Response, AppError> {
    let (ctx, t) = match resolve_managed(&state, &headers, user.as_ref(), &name).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let Some(cap) = Capability::parse(&form.capability) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "unknown capability").into_response());
    };
    let value = match form.value.as_str() {
        "allow" => Some(true),
        "deny" => Some(false),
        "inherit" => None,
        _ => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                "value: allow, deny or inherit",
            )
                .into_response());
        }
    };
    if let Err(reason) = may_override(&ctx, cap, value == Some(true)) {
        return Ok(refuse(reason));
    }
    match value {
        Some(allowed) => {
            sqlx::query!(
                "INSERT INTO user_capabilities (user_id, wiki_id, capability, allowed, set_by)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (user_id, wiki_id, capability)
                 DO UPDATE SET allowed = EXCLUDED.allowed, set_by = EXCLUDED.set_by, set_at = now()",
                t.id,
                ctx.wiki.id,
                cap.as_str(),
                allowed,
                ctx.actor.user_id
            )
            .execute(&state.db)
            .await?;
        }
        None => {
            sqlx::query!(
                "DELETE FROM user_capabilities WHERE user_id = $1 AND wiki_id = $2 AND capability = $3",
                t.id,
                ctx.wiki.id,
                cap.as_str()
            )
            .execute(&state.db)
            .await?;
        }
    }
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "admin.user.capability",
            entity_type: "user",
            entity_id: Some(t.id),
            meta: json!({ "capability": cap.as_str(), "value": form.value }),
        },
    )
    .await?;
    Ok(back(&t, "capability"))
}

#[derive(serde::Deserialize)]
pub struct SanctionForm {
    /// "mute", "ban" or "ban_install".
    kind: String,
    #[serde(default)]
    reason: String,
    /// Days until it ends on its own; 0 or empty for no end.
    #[serde(default)]
    days: String,
}

/// POST /admin/user/{name}/sanction
pub async fn add_sanction(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(form): Form<SanctionForm>,
) -> Result<Response, AppError> {
    let (ctx, t) = match resolve_managed(&state, &headers, user.as_ref(), &name).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let (kind, wiki_id) = match form.kind.as_str() {
        "mute" => ("mute", Some(ctx.wiki.id)),
        "ban" => ("ban", Some(ctx.wiki.id)),
        "ban_install" => {
            if !matches!(ctx.actor.global, GlobalRole::Root | GlobalRole::Staff) {
                return Ok(refuse("only install staff may ban from every wiki"));
            }
            ("ban", None)
        }
        _ => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                "kind: mute, ban or ban_install",
            )
                .into_response());
        }
    };
    let reason = form.reason.trim();
    // A sanction without a reason is a grudge. The person, and the next admin,
    // deserve to read why.
    if reason.is_empty() || reason.chars().count() > REASON_MAX {
        return Ok(pages::see_other(&format!(
            "/admin/user/{}?done=reason_needed",
            t.username
        )));
    }
    let days: i64 = form.days.trim().parse().unwrap_or(0).clamp(0, 3650);
    let expires = (days > 0).then(|| chrono::Utc::now() + chrono::Duration::days(days));
    let id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO sanctions (id, user_id, wiki_id, kind, reason, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id,
        t.id,
        wiki_id,
        kind,
        reason,
        ctx.actor.user_id,
        expires
    )
    .execute(&state.db)
    .await?;
    // An install ban takes effect now, not at the next sign-in.
    if wiki_id.is_none() {
        crate::auth::session::delete_others(&state, t.id, None).await?;
    }
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "admin.user.sanction",
            entity_type: "user",
            entity_id: Some(t.id),
            meta: json!({ "sanction": id, "kind": form.kind, "days": days, "reason": reason }),
        },
    )
    .await?;
    Ok(back(&t, "sanctioned"))
}

#[derive(serde::Deserialize)]
pub struct LiftForm {
    #[serde(default)]
    reason: String,
}

/// POST /admin/user/{name}/lift/{id}
pub async fn lift_sanction(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path((name, id)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<LiftForm>,
) -> Result<Response, AppError> {
    let (ctx, t) = match resolve_managed(&state, &headers, user.as_ref(), &name).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let Some(id) = pages::parse_uuid(&id) else {
        return Ok(crate::errors::not_found());
    };
    let Some(row) = sqlx::query!(
        "SELECT wiki_id FROM sanctions WHERE id = $1 AND user_id = $2 AND lifted_at IS NULL",
        id,
        t.id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    match row.wiki_id {
        None if !matches!(ctx.actor.global, GlobalRole::Root | GlobalRole::Staff) => {
            return Ok(refuse("only install staff may lift an install-wide ban"));
        }
        Some(w) if w != ctx.wiki.id => return Ok(crate::errors::not_found()),
        _ => {}
    }
    let reason: String = form.reason.trim().chars().take(REASON_MAX).collect();
    sqlx::query!(
        "UPDATE sanctions SET lifted_at = now(), lifted_by = $2, lift_reason = $3 WHERE id = $1",
        id,
        ctx.actor.user_id,
        (!reason.is_empty()).then_some(reason.clone())
    )
    .execute(&state.db)
    .await?;
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "admin.user.lift",
            entity_type: "user",
            entity_id: Some(t.id),
            meta: json!({ "sanction": id, "reason": reason }),
        },
    )
    .await?;
    Ok(back(&t, "lifted"))
}

#[derive(serde::Deserialize)]
pub struct NoteForm {
    #[serde(default)]
    body: String,
}

/// POST /admin/user/{name}/note
pub async fn add_note(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(form): Form<NoteForm>,
) -> Result<Response, AppError> {
    let (ctx, t) = match resolve_managed(&state, &headers, user.as_ref(), &name).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let body = form.body.trim();
    if body.is_empty() || body.chars().count() > NOTE_MAX {
        return Ok(back(&t, "note_empty"));
    }
    sqlx::query!(
        "INSERT INTO user_notes (id, user_id, wiki_id, author_id, body) VALUES ($1, $2, $3, $4, $5)",
        Uuid::new_v4(),
        t.id,
        ctx.wiki.id,
        ctx.actor.user_id,
        body
    )
    .execute(&state.db)
    .await?;
    Ok(back(&t, "noted"))
}

/// POST /admin/user/{name}/verify-email
pub async fn verify_email(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let (ctx, t) = match resolve_managed(&state, &headers, user.as_ref(), &name).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let updated = sqlx::query!(
        "UPDATE users SET email_verified_at = now()
         WHERE id = $1 AND email IS NOT NULL AND email_verified_at IS NULL",
        t.id
    )
    .execute(&state.db)
    .await?
    .rows_affected();
    if updated > 0 {
        audit::record(
            &state.db,
            audit::Entry {
                wiki_id: Some(ctx.wiki.id),
                user_id: ctx.actor.user_id,
                action: "admin.user.verify_email",
                entity_type: "user",
                entity_id: Some(t.id),
                meta: json!({}),
            },
        )
        .await?;
    }
    Ok(back(&t, "verified"))
}

/// POST /admin/user/{name}/end-sessions
pub async fn end_sessions(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let (ctx, t) = match resolve_managed(&state, &headers, user.as_ref(), &name).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let ended = crate::auth::session::delete_others(&state, t.id, None).await?;
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "admin.user.end_sessions",
            entity_type: "user",
            entity_id: Some(t.id),
            meta: json!({ "ended": ended }),
        },
    )
    .await?;
    Ok(back(&t, "sessions_ended"))
}

#[derive(serde::Deserialize)]
pub struct CuratorForm {
    /// The curator's username, or empty for none.
    #[serde(default)]
    curator: String,
}

/// POST /admin/user/{name}/curator
pub async fn set_curator(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(form): Form<CuratorForm>,
) -> Result<Response, AppError> {
    let (ctx, t) = match resolve_managed(&state, &headers, user.as_ref(), &name).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    if !ctx.actor.can(Capability::UserRoleManage) {
        return Ok(refuse("assigning curators needs admin rights"));
    }
    let wanted = form.curator.trim().to_lowercase();
    if wanted.is_empty() {
        sqlx::query!(
            "DELETE FROM curatorships WHERE wiki_id = $1 AND user_id = $2",
            ctx.wiki.id,
            t.id
        )
        .execute(&state.db)
        .await?;
    } else {
        // Only someone who is a curator or above on this wiki may look after
        // people here, and nobody curates themselves.
        let Some(curator_id) = sqlx::query_scalar!(
            "SELECT u.id FROM users u JOIN wiki_memberships m ON m.user_id = u.id
             WHERE lower(u.username) = $1 AND m.wiki_id = $2
               AND m.role IN ('curator', 'moderator', 'admin', 'owner')",
            wanted,
            ctx.wiki.id
        )
        .fetch_optional(&state.db)
        .await?
        .filter(|id| *id != t.id) else {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                "curator: not a curator on this wiki",
            )
                .into_response());
        };
        sqlx::query!(
            "INSERT INTO curatorships (wiki_id, user_id, curator_id, assigned_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (wiki_id, user_id)
             DO UPDATE SET curator_id = EXCLUDED.curator_id, assigned_by = EXCLUDED.assigned_by,
                           assigned_at = now()",
            ctx.wiki.id,
            t.id,
            curator_id,
            ctx.actor.user_id
        )
        .execute(&state.db)
        .await?;
    }
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "admin.user.curator",
            entity_type: "user",
            entity_id: Some(t.id),
            meta: json!({ "curator": (!wanted.is_empty()).then_some(wanted) }),
        },
    )
    .await?;
    Ok(back(&t, "curator"))
}
