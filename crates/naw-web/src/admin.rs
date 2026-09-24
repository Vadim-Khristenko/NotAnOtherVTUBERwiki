//! The admin panel: one template with sections.
//!
//! Every mutation is a POST. `SameSite=Lax` keeps the session cookie off
//! cross-site POSTs, which is the CSRF defence here, and it only holds while
//! nothing that changes state is reachable by GET.

use axum::extract::{Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tracing::instrument;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::audit;
use crate::auth::session::CurrentUser;
use crate::pages::{self, ENGINE_VERSION};
use crate::perm::{Capability, GlobalRole, WikiRole};
use crate::resolve::{Ctx, context};

const PER_PAGE: i64 = 50;
const HTML: (header::HeaderName, &str) = (header::CONTENT_TYPE, "text/html; charset=utf-8");

/// Resolves the request and refuses anybody without `AdminPanel`, returning
/// the rendered refusal as `Err`.
#[allow(clippy::result_large_err)]
pub(crate) async fn gate(
    state: &AppState,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
) -> Result<Ctx, Response> {
    gate_for(state, headers, user, Capability::AdminPanel).await
}

/// As [`gate`], for a section that needs `cap` rather than the whole panel:
/// moderators reach the report queue without reaching the settings.
#[allow(clippy::result_large_err)]
pub(crate) async fn gate_for(
    state: &AppState,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
    cap: Capability,
) -> Result<Ctx, Response> {
    let ctx = match context(state, headers, user).await {
        Ok(Some(ctx)) => ctx,
        Ok(None) => {
            return Err((StatusCode::NOT_FOUND, "no wiki for this host").into_response());
        }
        Err(err) => return Err(err.into_response()),
    };
    if !ctx.actor.can(cap) {
        // A guest is sent to sign in; anyone else is told no.
        if !ctx.actor.is_signed_in() {
            return Err(
                pages::redirect_response(StatusCode::SEE_OTHER, "/login?next=/admin")
                    .unwrap_or_else(|| (StatusCode::FORBIDDEN, "sign in first").into_response()),
            );
        }
        return Err((StatusCode::FORBIDDEN, "the admin panel needs admin rights").into_response());
    }
    Ok(ctx)
}

/// Renders `admin.html` for one section.
pub(crate) fn render(
    ctx: &Ctx,
    section: &str,
    heading: &str,
    extra: minijinja::Value,
) -> Result<Response, AppError> {
    let template = ctx
        .skin
        .env
        .get_template("admin.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t_with("admin.suffix", &[("section", heading)]),
                version => ENGINE_VERSION,
                section => section,
                heading => heading,
                is_root => ctx.actor.global == GlobalRole::Root,
                can_panel => ctx.actor.can(Capability::AdminPanel),
                can_reports => ctx.actor.can(Capability::ReportHandle),
                can_audit => ctx.actor.can(Capability::AuditRead),
                my_role => ctx.actor.effective_role().map(WikiRole::as_str),
            },
            ..extra
        })
        .map_err(pages::template_error)?;
    Ok((StatusCode::OK, [HTML], html).into_response())
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

/// GET /admin
#[instrument(skip(state, user, headers))]
pub async fn overview(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    let stats = sqlx::query!(
        r#"
        SELECT
          (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL) AS "live!",
          (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NOT NULL) AS "archived!",
          (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND is_locked) AS "locked!",
          (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id
             WHERE p.wiki_id = $1) AS "revisions!",
          (SELECT count(*) FROM wiki_memberships WHERE wiki_id = $1) AS "members!",
          (SELECT count(*) FROM users) AS "accounts!",
          (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main'
             AND deleted_at IS NULL AND search_vector IS NULL) AS "unindexed!"
        "#,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?;

    let recent = sqlx::query!(
        r#"
        SELECT a.action, a.created_at, a.meta, u.username AS "actor?"
        FROM audit_log a
        LEFT JOIN users u ON u.id = a.user_id
        WHERE a.wiki_id = $1
        ORDER BY a.created_at DESC
        LIMIT 10
        "#,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let events: Vec<minijinja::Value> = recent
        .into_iter()
        .map(|row| {
            minijinja::context! {
                action => row.action,
                actor => row.actor,
                at => row.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                meta => row.meta.to_string(),
            }
        })
        .collect();

    render(
        &ctx,
        "overview",
        &ctx.t("admin.overview"),
        minijinja::context! {
            wiki_slug => ctx.wiki.slug.clone(),
            wiki_domain => ctx.wiki.domain.clone(),
            live => stats.live,
            archived => stats.archived,
            locked => stats.locked,
            revisions => stats.revisions,
            members => stats.members,
            accounts => stats.accounts,
            unindexed => stats.unindexed,
            events => events,
        },
    )
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    page: Option<i64>,
}

/// GET /admin/users
#[instrument(skip(state, user, headers))]
pub async fn users(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    let page_no = query.page.unwrap_or(1).max(1);
    let offset = (page_no - 1) * PER_PAGE;
    // Empty means everybody; otherwise a substring match.
    let filter = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", q.to_lowercase()));

    let rows = sqlx::query!(
        r#"
        SELECT u.id, u.username, u.global_role, u.created_at,
               u.must_change_password,
               (u.email_verified_at IS NOT NULL) AS "email_verified!",
               m.role::text AS "wiki_role?",
               (SELECT count(*) FROM oauth_identities oi WHERE oi.user_id = u.id) AS "identities!"
        FROM users u
        LEFT JOIN wiki_memberships m ON m.user_id = u.id AND m.wiki_id = $1
        WHERE ($2::text IS NULL OR lower(u.username) LIKE $2)
        ORDER BY u.created_at DESC
        LIMIT $3 OFFSET $4
        "#,
        ctx.wiki.id,
        filter,
        PER_PAGE,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    let total = sqlx::query!(
        r#"SELECT count(*) AS "count!" FROM users u
           WHERE ($1::text IS NULL OR lower(u.username) LIKE $1)"#,
        filter
    )
    .fetch_one(&state.db)
    .await?
    .count;

    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            let effective = row.wiki_role.as_deref().unwrap_or("registered");
            minijinja::context! {
                id => row.id.to_string(),
                username => row.username,
                // Parsed, so a value outside the enum shows as "registered".
                global_role => GlobalRole::parse(&row.global_role).as_str(),
                wiki_role => row.wiki_role.clone(),
                effective_role => effective,
                email_verified => row.email_verified,
                identities => row.identities,
                created_at => row.created_at.format("%Y-%m-%d").to_string(),
                is_me => Some(row.id) == ctx.actor.user_id,
                temporary => row.must_change_password,
            }
        })
        .collect();

    render(
        &ctx,
        "users",
        &ctx.t("admin.accounts"),
        minijinja::context! {
            users => items,
            total => total,
            query => query.q.clone().unwrap_or_default(),
            page_no => page_no,
            has_prev => page_no > 1,
            has_next => offset + PER_PAGE < total,
            prev_page => page_no - 1,
            next_page => page_no + 1,
            can_create => ctx.actor.manages_accounts(),
        },
    )
}

#[derive(Debug, serde::Deserialize)]
pub struct RoleForm {
    user_id: String,
    /// A `WikiRole` name, or empty to remove the membership.
    role: String,
    /// Where to return: only an admin user page is honoured.
    #[serde(default)]
    back: String,
}

/// POST /admin/users/role
#[instrument(skip(state, user, headers, form))]
pub async fn set_role(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<RoleForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.manages_accounts() {
        return Ok((StatusCode::FORBIDDEN, ctx.t("admin.why_needs_admin")).into_response());
    }
    let Some(target_id) = pages::parse_uuid(&form.user_id) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "user_id: expected an id").into_response());
    };
    // Nobody demotes themselves, least of all the last admin.
    if Some(target_id) == ctx.actor.user_id {
        return Ok((StatusCode::CONFLICT, "you cannot change your own role here").into_response());
    }

    // Demoting a peer is refused like promoting to one, so no admin can take
    // sole control.
    let current = sqlx::query!(
        r#"SELECT role::text AS role FROM wiki_memberships WHERE user_id = $1 AND wiki_id = $2"#,
        target_id,
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?
    .and_then(|row| row.role)
    .as_deref()
    .and_then(WikiRole::parse);
    if let Some(held) = current
        && !ctx.actor.may_grant(held)
    {
        return Ok((
            StatusCode::FORBIDDEN,
            "that account holds a role at or above your own",
        )
            .into_response());
    }

    let requested = form.role.trim();
    if requested.is_empty() {
        sqlx::query!(
            "DELETE FROM wiki_memberships WHERE user_id = $1 AND wiki_id = $2",
            target_id,
            ctx.wiki.id
        )
        .execute(&state.db)
        .await?;
        audit::record(
            &state.db,
            audit::Entry {
                wiki_id: Some(ctx.wiki.id),
                user_id: ctx.actor.user_id,
                action: "membership.remove",
                entity_type: "user",
                entity_id: Some(target_id),
                meta: json!({ "was": current.map(WikiRole::as_str) }),
            },
        )
        .await?;
        return Ok(pages::see_other(role_back(&form.back)));
    }

    let Some(role) = WikiRole::parse(requested) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "role: not a known role").into_response());
    };
    if !ctx.actor.may_grant(role) {
        return Ok((
            StatusCode::FORBIDDEN,
            "you cannot grant a role at or above your own",
        )
            .into_response());
    }

    // `role` came from `WikiRole`, so only enum spellings reach the cast.
    sqlx::query(
        "INSERT INTO wiki_memberships (user_id, wiki_id, role)
         VALUES ($1, $2, $3::user_wiki_role)
         ON CONFLICT (user_id, wiki_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(target_id)
    .bind(ctx.wiki.id)
    .bind(role.as_str())
    .execute(&state.db)
    .await?;
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "membership.set",
            entity_type: "user",
            entity_id: Some(target_id),
            meta: json!({ "was": current.map(WikiRole::as_str), "now": role.as_str() }),
        },
    )
    .await?;
    Ok(pages::see_other(role_back(&form.back)))
}

/// The admin user page a role change came from, or the account list.
fn role_back(back: &str) -> &str {
    let ok = back.strip_prefix("/admin/user/").is_some_and(|name| {
        !name.is_empty()
            && name.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
            })
    });
    if ok { back } else { "/admin/users" }
}

// ---------------------------------------------------------------------------
// Accounts made by an admin
// ---------------------------------------------------------------------------
//
// The temporary password is shown once, never logged or stored in plain
// text, and must be replaced at the first sign-in.

/// Why this admin may not act on that account on this wiki, as a message key.
pub(crate) fn may_manage(
    ctx: &Ctx,
    target: uuid::Uuid,
    target_global: GlobalRole,
    target_role: Option<WikiRole>,
) -> Result<(), &'static str> {
    if !ctx.actor.manages_accounts() {
        return Err("admin.why_needs_admin");
    }
    // Your own account is changed in settings, which ask for your password.
    if Some(target) == ctx.actor.user_id {
        return Err("admin.why_own_password");
    }
    match target_global {
        GlobalRole::Root => return Err("admin.why_root"),
        GlobalRole::Staff if ctx.actor.global != GlobalRole::Root => {
            return Err("admin.why_staff");
        }
        _ => {}
    }
    if let Some(held) = target_role
        && !ctx.actor.may_grant(held)
    {
        return Err("admin.why_higher_role");
    }
    Ok(())
}

/// Why this admin may not touch what the account carries to every wiki of
/// the install: its password, email, sessions and avatar. The role here is
/// not enough. Someone who never joined this wiki, or who holds rights on
/// another one, is not this wiki's to take over.
pub(crate) async fn may_manage_account(
    state: &AppState,
    ctx: &Ctx,
    target: uuid::Uuid,
    target_global: GlobalRole,
    target_role: Option<WikiRole>,
) -> Result<Result<(), &'static str>, AppError> {
    if let Err(key) = may_manage(ctx, target, target_global, target_role) {
        return Ok(Err(key));
    }
    if ctx.actor.global == GlobalRole::Root {
        return Ok(Ok(()));
    }
    if target_role.is_none() {
        return Ok(Err("admin.why_not_member"));
    }
    let elsewhere = sqlx::query_scalar!(
        r#"SELECT role::text AS "role!" FROM wiki_memberships WHERE user_id = $1 AND wiki_id <> $2"#,
        target,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    if elsewhere
        .iter()
        .filter_map(|role| WikiRole::parse(role))
        .any(|role| role >= WikiRole::Curator)
    {
        return Ok(Err("admin.why_rights_elsewhere"));
    }
    Ok(Ok(()))
}

/// One `@`, something on both sides, no spaces, a sane length.
fn email_is_plausible(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    email.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.contains(char::is_whitespace)
        && !domain.contains('@')
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct NewUserForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
    /// The admin vouches for the address, so a provider sign-in with the same
    /// verified address links to this account.
    #[serde(default)]
    trust_email: Option<String>,
    /// A `WikiRole` name, or empty for no membership.
    #[serde(default)]
    role: String,
}

fn render_new_user(
    ctx: &Ctx,
    status: StatusCode,
    form: &NewUserForm,
    error: Option<&str>,
) -> Result<Response, AppError> {
    let grantable: Vec<&str> = WikiRole::ALL
        .iter()
        .filter(|role| ctx.actor.may_grant(**role))
        .map(|role| role.as_str())
        .collect();
    let mut response = render(
        ctx,
        "user_new",
        &ctx.t("admin.user_new"),
        minijinja::context! {
            grantable => grantable,
            form_username => form.username.clone(),
            form_email => form.email.clone(),
            form_trust => form.trust_email.is_some(),
            form_role => form.role.clone(),
            error => error.map(|key| ctx.t(&format!("admin.{key}"))),
        },
    )?;
    *response.status_mut() = status;
    Ok(response)
}

/// GET /admin/users/new
#[instrument(skip(state, user, headers))]
pub async fn new_user(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.manages_accounts() {
        return Ok((StatusCode::FORBIDDEN, ctx.t("admin.why_needs_admin")).into_response());
    }
    render_new_user(&ctx, StatusCode::OK, &NewUserForm::default(), None)
}

/// Shows a freshly issued temporary password, once.
fn render_issued(
    ctx: &Ctx,
    username: &str,
    password: &str,
    created: bool,
) -> Result<Response, AppError> {
    render(
        ctx,
        "password_issued",
        &ctx.t(if created {
            "admin.user_created"
        } else {
            "admin.password_reset_done"
        }),
        minijinja::context! {
            issued_username => username,
            issued_password => password,
            created => created,
        },
    )
}

/// POST /admin/users/new
#[instrument(skip(state, user, form, headers))]
pub async fn create_user(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<NewUserForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.manages_accounts() {
        return Ok((StatusCode::FORBIDDEN, ctx.t("admin.why_needs_admin")).into_response());
    }
    let username = form.username.trim().to_lowercase();
    let email = form.email.trim().to_lowercase();
    let refuse = |status, key| render_new_user(&ctx, status, &form, Some(key));

    // Reserved names are allowed: only an admin may hand them out, and this is one.
    if !crate::auth::username::is_valid(&username) {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "user_bad_name");
    }
    if !email.is_empty() && !email_is_plausible(&email) {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "user_bad_email");
    }
    let role = match form.role.trim() {
        "" => None,
        name => match WikiRole::parse(name) {
            Some(role) if ctx.actor.may_grant(role) => Some(role),
            _ => return refuse(StatusCode::FORBIDDEN, "user_bad_role"),
        },
    };

    let temporary = crate::auth::password::temporary();
    let hash = crate::auth::password::hash(temporary.clone()).await?;
    let user_id = uuid::Uuid::new_v4();
    let email = (!email.is_empty()).then_some(email);
    let verified_at = email
        .as_ref()
        .filter(|_| form.trust_email.is_some())
        .map(|_| chrono::Utc::now());

    let mut tx = state.db.begin().await?;
    let taken = sqlx::query!(
        "SELECT 1 AS one FROM users WHERE lower(username) = $1
         UNION ALL
         SELECT 1 AS one FROM user_aliases WHERE lower(alias) = $1",
        username
    )
    .fetch_optional(&mut *tx)
    .await?
    .is_some();
    if taken {
        return refuse(StatusCode::CONFLICT, "user_name_taken");
    }
    if let Some(email) = &email {
        let used = sqlx::query!("SELECT 1 AS one FROM users WHERE lower(email) = $1", email)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
        if used {
            return refuse(StatusCode::CONFLICT, "user_email_taken");
        }
    }
    sqlx::query!(
        // No locale: the account follows its owner's browser until they choose.
        "INSERT INTO users (id, username, email, email_verified_at, password_hash,
                            must_change_password, created_by, global_role)
         VALUES ($1, $2, $3, $4, $5, true, $6, 'registered')",
        user_id,
        username,
        email,
        verified_at,
        hash,
        ctx.actor.user_id
    )
    .execute(&mut *tx)
    .await?;
    if let Some(role) = role {
        sqlx::query(
            "INSERT INTO wiki_memberships (user_id, wiki_id, role)
             VALUES ($1, $2, $3::user_wiki_role)",
        )
        .bind(user_id)
        .bind(ctx.wiki.id)
        .bind(role.as_str())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "admin.user_create",
            entity_type: "user",
            entity_id: Some(user_id),
            meta: json!({
                "username": username,
                "role": role.map(WikiRole::as_str),
                "email_trusted": verified_at.is_some(),
            }),
        },
    )
    .await?;
    tracing::info!(%username, "account created by an admin");
    render_issued(&ctx, &username, &temporary, true)
}

#[derive(Debug, serde::Deserialize)]
pub struct ResetForm {
    user_id: String,
}

/// POST /admin/users/password
#[instrument(skip(state, user, headers, form))]
pub async fn reset_password(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<ResetForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    let Some(target_id) = pages::parse_uuid(&form.user_id) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "user_id: expected an id").into_response());
    };
    let Some(target) = sqlx::query!(
        r#"SELECT u.username, u.global_role, m.role::text AS "wiki_role?"
           FROM users u
           LEFT JOIN wiki_memberships m ON m.user_id = u.id AND m.wiki_id = $2
           WHERE u.id = $1"#,
        target_id,
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    let held = target.wiki_role.as_deref().and_then(WikiRole::parse);
    if let Err(reason) = may_manage_account(
        &state,
        &ctx,
        target_id,
        GlobalRole::parse(&target.global_role),
        held,
    )
    .await?
    {
        return Ok((StatusCode::FORBIDDEN, ctx.t(reason)).into_response());
    }

    let temporary = crate::auth::password::temporary();
    let hash = crate::auth::password::hash(temporary.clone()).await?;
    sqlx::query!(
        "UPDATE users SET password_hash = $2, must_change_password = true WHERE id = $1",
        target_id,
        hash
    )
    .execute(&state.db)
    .await?;
    // Existing sessions would outlive the reset; end them all.
    let ended = crate::auth::session::delete_others(&state, target_id, None).await?;
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "admin.password_reset",
            entity_type: "user",
            entity_id: Some(target_id),
            meta: json!({ "username": target.username, "sessions_ended": ended }),
        },
    )
    .await?;
    render_issued(&ctx, &target.username, &temporary, false)
}

// ---------------------------------------------------------------------------
// Header and footer
// ---------------------------------------------------------------------------

/// GET /admin/chrome
#[instrument(skip(state, user, headers))]
pub async fn chrome_settings(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<SavedQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.can(Capability::WikiSettings) {
        return Ok((StatusCode::FORBIDDEN, "wiki settings need admin rights").into_response());
    }
    let header = crate::chrome::header(&ctx.wiki.settings);
    let saved_links = crate::chrome::footer(&ctx.wiki.settings);
    let customised = saved_links.is_some();
    // Always MAX rows, so adding a link needs no script.
    let mut rows: Vec<minijinja::Value> = saved_links
        .unwrap_or_default()
        .into_iter()
        .map(|l| minijinja::context! { href => l.href, label => l.label, lang => l.lang })
        .collect();
    while rows.len() < crate::chrome::MAX_FOOTER_LINKS {
        rows.push(minijinja::context! { href => "", label => "", lang => "" });
    }
    render(
        &ctx,
        "chrome",
        &ctx.t("admin.chrome"),
        minijinja::context! {
            h_new_page => header.new_page,
            h_about => header.about,
            h_languages => header.languages,
            h_theme => header.theme,
            h_search => header.search,
            h_emotes => header.emotes,
            footer_rows => rows,
            footer_customised => customised,
            offered => ctx.offered_languages(),
            notices => {
                let saved = crate::chrome::notices(&ctx.wiki.settings);
                ctx.offered_languages()
                    .into_iter()
                    .map(|code| minijinja::context! {
                        code => code.clone(),
                        name => crate::translate::native_name(&ctx, &code),
                        text => saved.get(&code).and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                    .collect::<Vec<_>>()
            },
            domains => crate::chrome::domains(&ctx.wiki.settings).join(", "),
            notice_max => crate::chrome::NOTICE_MAX,
            saved => query.saved.is_some(),
            refused => query.refused.filter(|n| *n > 0),
        },
    )
}

/// POST /admin/chrome
#[instrument(skip(state, user, form, headers))]
pub async fn save_chrome_settings(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.can(Capability::WikiSettings) {
        return Ok((StatusCode::FORBIDDEN, "wiki settings need admin rights").into_response());
    }
    let on = |key: &str| form.contains_key(key);
    let header = json!({
        "new_page": on("h_new_page"),
        "about": on("h_about"),
        "languages": on("h_languages"),
        "theme": on("h_theme"),
        "search": on("h_search"),
        "emotes": on("h_emotes"),
    });
    let offered = ctx.offered_languages();
    let mut links = Vec::new();
    let mut refused = 0;
    for i in 0..crate::chrome::MAX_FOOTER_LINKS {
        let get = |name: &str| {
            form.get(&format!("{name}_{i}"))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        let (href, label, lang) = (get("href"), get("label"), get("lang").to_ascii_lowercase());
        if href.is_empty() && label.is_empty() {
            continue;
        }
        if !crate::chrome::href_is_safe(&href) || label.is_empty() || label.chars().count() > 60 {
            refused += 1;
            continue;
        }
        let lang = if offered.contains(&lang) {
            lang
        } else {
            String::new()
        };
        links.push(json!({ "href": href, "label": label, "lang": lang }));
    }
    let footer = if on("footer_reset") {
        Value::Null
    } else {
        Value::Array(links)
    };
    let mut notice = serde_json::Map::new();
    for code in &offered {
        let text = form
            .get(&format!("notice_{code}"))
            .map(|v| v.trim().replace("\r\n", "\n"))
            .unwrap_or_default();
        if !text.is_empty() {
            let text: String = text.chars().take(crate::chrome::NOTICE_MAX).collect();
            notice.insert(code.clone(), Value::String(text));
        }
    }
    let raw_domains = form.get("domains").map(String::as_str).unwrap_or("");
    let domains: Vec<String> = raw_domains
        .split([',', ' ', '\n'])
        .filter(|d| !d.trim().is_empty())
        .filter_map(|d| {
            let clean = crate::chrome::clean_domain(d);
            if clean.is_none() {
                refused += 1;
            }
            clean
        })
        .take(crate::chrome::MAX_DOMAINS)
        .collect();

    let mut settings = ctx.wiki.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    let object = settings.as_object_mut().expect("just ensured an object");
    let mut chrome = object
        .get("chrome")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    chrome["header"] = header.clone();
    chrome["notice"] = Value::Object(notice);
    chrome["domains"] = json!(domains);
    if footer.is_null() {
        if let Some(map) = chrome.as_object_mut() {
            map.remove("footer");
        }
    } else {
        chrome["footer"] = footer.clone();
    }
    object.insert("chrome".to_string(), chrome);
    sqlx::query!(
        "UPDATE wikis SET settings = $2 WHERE id = $1",
        ctx.wiki.id,
        settings
    )
    .execute(&state.db)
    .await?;
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "wiki.chrome",
            entity_type: "wiki",
            entity_id: Some(ctx.wiki.id),
            meta: json!({ "header": header, "footer": footer, "domains": domains, "refused_links": refused }),
        },
    )
    .await?;
    let target = if refused > 0 {
        format!("/admin/chrome?saved=1&refused={refused}")
    } else {
        "/admin/chrome?saved=1".to_string()
    };
    Ok(pages::see_other(&target))
}

// ---------------------------------------------------------------------------
// Install: account rules
// ---------------------------------------------------------------------------
//
// Accounts are shared by every wiki, so these belong to the install owner.

/// GET /admin/accounts
#[instrument(skip(state, user, headers))]
pub async fn account_rules(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<SavedQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if ctx.actor.global != GlobalRole::Root {
        return Ok((
            StatusCode::FORBIDDEN,
            "account rules belong to the install owner",
        )
            .into_response());
    }
    let policy = crate::policy::accounts(&state).await?;
    let defaults = state.config.accounts;
    render(
        &ctx,
        "accounts",
        &ctx.t("admin.accounts_rules"),
        minijinja::context! {
            rename_enabled => policy.rename_enabled,
            aliases_disabled => policy.aliases_disabled,
            rename_cooldown_days => policy.rename_cooldown_days,
            alias_days => policy.alias_days,
            max_aliases => policy.max_aliases,
            default_cooldown => defaults.rename_cooldown_days,
            default_alias_days => defaults.alias_days,
            default_max_aliases => defaults.max_aliases,
            saved => query.saved.is_some(),
        },
    )
}

#[derive(Debug, serde::Deserialize)]
pub struct SavedQuery {
    #[serde(default)]
    saved: Option<String>,
    /// Submitted rows dropped as unsafe or incomplete.
    #[serde(default)]
    refused: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
pub struct AccountRulesForm {
    #[serde(default)]
    rename_enabled: Option<String>,
    #[serde(default)]
    aliases_disabled: Option<String>,
    #[serde(default)]
    rename_cooldown_days: i64,
    #[serde(default)]
    alias_days: i64,
    #[serde(default)]
    max_aliases: i64,
}

/// POST /admin/accounts
#[instrument(skip(state, user, headers, form))]
pub async fn save_account_rules(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<AccountRulesForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if ctx.actor.global != GlobalRole::Root {
        return Ok((
            StatusCode::FORBIDDEN,
            "account rules belong to the install owner",
        )
            .into_response());
    }
    let before = crate::policy::accounts(&state).await?;
    let after = naw_core::config::AccountPolicy {
        rename_enabled: form.rename_enabled.is_some(),
        aliases_disabled: form.aliases_disabled.is_some(),
        rename_cooldown_days: form.rename_cooldown_days,
        alias_days: form.alias_days,
        max_aliases: form.max_aliases,
    }
    .clamped();
    crate::policy::save_accounts(&state, after, ctx.actor.user_id).await?;
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: None,
            user_id: ctx.actor.user_id,
            action: "install.account_rules",
            entity_type: "install",
            entity_id: None,
            meta: json!({ "was": before, "now": after }),
        },
    )
    .await?;
    Ok(pages::see_other("/admin/accounts?saved=1"))
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// GET /admin/pages
#[instrument(skip(state, user, headers))]
pub async fn pages_list(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    let page_no = query.page.unwrap_or(1).max(1);
    let offset = (page_no - 1) * PER_PAGE;
    let filter = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", q.to_lowercase()));

    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.slug, p.title, p.is_locked, p.updated_at, p.deleted_at,
               (p.search_vector IS NOT NULL) AS "indexed!",
               (SELECT count(*) FROM revisions r WHERE r.page_id = p.id) AS "revisions!"
        FROM pages p
        WHERE p.wiki_id = $1 AND p.namespace = 'main'
          AND ($2::text IS NULL OR lower(p.slug) LIKE $2 OR lower(p.title) LIKE $2)
        ORDER BY p.updated_at DESC
        LIMIT $3 OFFSET $4
        "#,
        ctx.wiki.id,
        filter,
        PER_PAGE,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    let total = sqlx::query!(
        r#"SELECT count(*) AS "count!" FROM pages p WHERE p.wiki_id = $1 AND p.namespace = 'main'
           AND ($2::text IS NULL OR lower(p.slug) LIKE $2 OR lower(p.title) LIKE $2)"#,
        ctx.wiki.id,
        filter
    )
    .fetch_one(&state.db)
    .await?
    .count;

    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            minijinja::context! {
                id => row.id.to_string(),
                slug => row.slug,
                title => row.title,
                locked => row.is_locked,
                archived => row.deleted_at.is_some(),
                indexed => row.indexed,
                revisions => row.revisions,
                updated_at => row.updated_at.format("%Y-%m-%d %H:%M").to_string(),
            }
        })
        .collect();

    render(
        &ctx,
        "pages",
        &ctx.t("admin.pages"),
        minijinja::context! {
            pages => items,
            total => total,
            query => query.q.clone().unwrap_or_default(),
            page_no => page_no,
            has_prev => page_no > 1,
            has_next => offset + PER_PAGE < total,
            prev_page => page_no - 1,
            next_page => page_no + 1,
        },
    )
}

#[derive(Debug, serde::Deserialize)]
pub struct PageActionForm {
    page_id: String,
}

/// The four page actions, sharing one handler and one permission check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageAction {
    Lock,
    Unlock,
    Archive,
    Restore,
}

impl PageAction {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "lock" => Some(Self::Lock),
            "unlock" => Some(Self::Unlock),
            "archive" => Some(Self::Archive),
            "restore" => Some(Self::Restore),
            _ => None,
        }
    }

    fn capability(self) -> Capability {
        match self {
            Self::Lock | Self::Unlock => Capability::PageLock,
            Self::Archive | Self::Restore => Capability::PageDelete,
        }
    }

    fn audit_action(self) -> &'static str {
        match self {
            Self::Lock => "page.lock",
            Self::Unlock => "page.unlock",
            Self::Archive => "page.archive",
            Self::Restore => "page.restore",
        }
    }
}

/// POST /admin/pages/{action}
#[instrument(skip(state, user, headers, form))]
pub async fn page_action(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(action): Path<String>,
    headers: HeaderMap,
    Form(form): Form<PageActionForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    let Some(action) = PageAction::parse(&action) else {
        return Ok((StatusCode::NOT_FOUND, "no such action").into_response());
    };
    if !ctx.actor.can(action.capability()) {
        return Ok((StatusCode::FORBIDDEN, "not allowed").into_response());
    }
    let Some(page_id) = pages::parse_uuid(&form.page_id) else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "page_id: expected an id").into_response());
    };

    // `wiki_id` in the WHERE clause is the tenancy check.
    let slug = match action {
        PageAction::Lock | PageAction::Unlock => {
            // "Lock" is protection at moderator level, under the usual ladder.
            let Some(current) = sqlx::query!(
                "SELECT is_locked, edit_level FROM pages WHERE id = $1 AND wiki_id = $2",
                page_id,
                ctx.wiki.id
            )
            .fetch_optional(&state.db)
            .await?
            else {
                return Ok((StatusCode::NOT_FOUND, "nothing to change").into_response());
            };
            let from = pages::protection_of(current.is_locked, current.edit_level.as_deref());
            let to = (action == PageAction::Lock).then_some(WikiRole::Moderator);
            if !ctx.actor.may_protect(from, to) {
                return Ok(
                    (StatusCode::FORBIDDEN, "that protection is above your role").into_response(),
                );
            }
            sqlx::query!(
                "UPDATE pages SET is_locked = $3, edit_level = $4 WHERE id = $1 AND wiki_id = $2 RETURNING slug",
                page_id,
                ctx.wiki.id,
                to.is_some(),
                to.map(WikiRole::as_str)
            )
            .fetch_optional(&state.db)
            .await?
            .map(|row| row.slug)
        }
        PageAction::Archive => sqlx::query!(
            "UPDATE pages SET deleted_at = now(), deleted_by = $3
             WHERE id = $1 AND wiki_id = $2 AND deleted_at IS NULL RETURNING slug",
            page_id,
            ctx.wiki.id,
            ctx.actor.user_id
        )
        .fetch_optional(&state.db)
        .await?
        .map(|row| row.slug),
        PageAction::Restore => sqlx::query!(
            "UPDATE pages SET deleted_at = NULL, deleted_by = NULL
             WHERE id = $1 AND wiki_id = $2 AND deleted_at IS NOT NULL RETURNING slug",
            page_id,
            ctx.wiki.id
        )
        .fetch_optional(&state.db)
        .await?
        .map(|row| row.slug),
    };
    let Some(slug) = slug else {
        // Not on this wiki, or already in that state.
        return Ok((StatusCode::NOT_FOUND, "nothing to change").into_response());
    };

    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: action.audit_action(),
            entity_type: "page",
            entity_id: Some(page_id),
            meta: json!({ "slug": slug }),
        },
    )
    .await?;
    Ok(pages::see_other("/admin/pages"))
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

/// Groups the audit log can be narrowed to, each a set of action prefixes.
const AUDIT_GROUPS: [(&str, &[&str]); 6] = [
    ("pages", &["page.", "revision.", "profile."]),
    (
        "people",
        &["admin.", "membership.", "user.", "cli.", "grant."],
    ),
    ("sign_in", &["auth."]),
    ("files", &["media.", "emotes."]),
    ("reports", &["report."]),
    ("settings", &["wiki.", "install.", "skin.", "search."]),
];

#[derive(Debug, serde::Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    who: Option<String>,
    #[serde(default)]
    page: Option<i64>,
}

/// Where an audit row's subject lives, when it still has a page.
fn audit_subject(
    ctx: &Ctx,
    entity_type: &str,
    meta: &Value,
    subject_user: Option<&str>,
) -> Option<(String, String)> {
    let text = |key: &str| meta.get(key).and_then(Value::as_str);
    if let Some(name) = subject_user {
        return Some((format!("/user/{name}"), name.to_string()));
    }
    if let Some(name) = text("profile_of") {
        return Some((format!("/user/{name}"), name.to_string()));
    }
    match entity_type {
        "page" => text("slug").map(|slug| {
            let href = match text("locale") {
                Some(locale) => ctx.link_for(locale, &format!("/{slug}")),
                None => ctx.link(&format!("/{slug}")),
            };
            (href, slug.to_string())
        }),
        "media" => text("name").map(|name| (format!("/image:{name}"), name.to_string())),
        "report" => text("subject").map(|subject| (String::new(), subject.to_string())),
        _ => None,
    }
}

/// GET /admin/audit
#[instrument(skip(state, user, headers))]
pub async fn audit_log(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate_for(&state, &headers, user.as_ref(), Capability::AuditRead).await);
    let page_no = query.page.unwrap_or(1).max(1);
    let offset = (page_no - 1) * PER_PAGE;
    let filter = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", q.to_lowercase()));
    let group = query
        .group
        .as_deref()
        .and_then(|g| AUDIT_GROUPS.iter().find(|(id, _)| *id == g));
    let prefixes: Option<Vec<String>> =
        group.map(|(_, list)| list.iter().map(|p| format!("{p}%")).collect());
    let who = query
        .who
        .as_deref()
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase);
    let is_root = ctx.actor.global == GlobalRole::Root;

    // Install-wide rows (wiki_id NULL) are the sign-in events of every wiki on
    // the install. A wiki's staff see those of its own members only.
    let rows = sqlx::query!(
        r#"
        SELECT a.id, a.action, a.entity_type, a.entity_id, a.meta, a.created_at,
               (a.wiki_id IS NULL) AS "install_wide!",
               (SELECT u.username FROM users u WHERE u.id = a.user_id) AS actor,
               (SELECT u.username FROM users u
                 WHERE a.entity_type = 'user' AND u.id = a.entity_id) AS subject_user
        FROM audit_log a
        WHERE (a.wiki_id = $1
               OR (a.wiki_id IS NULL
                   AND ($5 OR EXISTS (SELECT 1 FROM wiki_memberships m
                                      WHERE m.wiki_id = $1
                                        AND m.user_id = COALESCE(a.user_id, a.entity_id)))))
          AND ($2::text IS NULL OR lower(a.action) LIKE $2)
          AND ($6::text[] IS NULL OR a.action LIKE ANY($6))
          AND ($7::text IS NULL OR a.user_id = (SELECT u.id FROM users u WHERE lower(u.username) = $7))
        ORDER BY a.created_at DESC
        LIMIT $3 OFFSET $4
        "#,
        ctx.wiki.id,
        filter,
        PER_PAGE,
        offset,
        is_root,
        prefixes.as_deref(),
        who
    )
    .fetch_all(&state.db)
    .await?;
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM audit_log a
           WHERE (a.wiki_id = $1
                  OR (a.wiki_id IS NULL
                      AND ($3 OR EXISTS (SELECT 1 FROM wiki_memberships m
                                         WHERE m.wiki_id = $1
                                           AND m.user_id = COALESCE(a.user_id, a.entity_id)))))
             AND ($2::text IS NULL OR lower(a.action) LIKE $2)
             AND ($4::text[] IS NULL OR a.action LIKE ANY($4))
             AND ($5::text IS NULL OR a.user_id = (SELECT u.id FROM users u WHERE lower(u.username) = $5))"#,
        ctx.wiki.id,
        filter,
        is_root,
        prefixes.as_deref(),
        who
    )
    .fetch_one(&state.db)
    .await?;

    let items: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            let key = format!("audit.{}", row.action.replace('.', "_"));
            let label = ctx.t(&key);
            let subject = audit_subject(
                &ctx,
                &row.entity_type,
                &row.meta,
                row.subject_user.as_deref(),
            );
            minijinja::context! {
                action => row.action.clone(),
                // An action with no wording yet shows as it is spelled.
                label => (label != key).then_some(label),
                entity_type => row.entity_type,
                actor => row.actor,
                install_wide => row.install_wide,
                day => ctx.day(row.created_at),
                time => row.created_at.format("%H:%M").to_string(),
                at => row.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                subject_href => subject.as_ref().map(|s| s.0.clone()).filter(|h| !h.is_empty()),
                subject_label => subject.map(|s| s.1),
                meta => (row.meta != serde_json::json!({})).then(|| row.meta.to_string()),
            }
        })
        .collect();
    let current_group = group.map(|(id, _)| *id).unwrap_or("");
    let groups = std::iter::once("")
        .chain(AUDIT_GROUPS.iter().map(|(id, _)| *id))
        .map(|id| {
            minijinja::context! {
                id => id,
                label => ctx.t(&format!("audit.group_{}", if id.is_empty() { "all" } else { id })),
                current => id == current_group,
            }
        })
        .collect::<Vec<_>>();

    render(
        &ctx,
        "audit",
        &ctx.t("admin.audit"),
        minijinja::context! {
            events => items,
            total => total,
            query => query.q.clone().unwrap_or_default(),
            who => query.who.clone().unwrap_or_default(),
            group => current_group,
            groups => groups,
            page_no => page_no,
            has_prev => page_no > 1,
            has_next => offset + PER_PAGE < total,
            prev_page => page_no - 1,
            next_page => page_no + 1,
        },
    )
}

// ---------------------------------------------------------------------------
// Wiki settings
// ---------------------------------------------------------------------------

/// GET /admin/wiki
#[instrument(skip(state, user, headers))]
pub async fn wiki_settings(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.can(Capability::WikiSettings) {
        return Ok((StatusCode::FORBIDDEN, "not allowed").into_response());
    }
    let rules = ctx.actor.rules;
    let home_slug = ctx
        .wiki
        .settings
        .get("home_slug")
        .and_then(Value::as_str)
        .unwrap_or("home")
        .to_string();
    render(
        &ctx,
        "wiki",
        &ctx.t("admin.settings"),
        minijinja::context! {
            wiki_slug => ctx.wiki.slug.clone(),
            wiki_domain => ctx.wiki.domain.clone().unwrap_or_default(),
            wiki_display_name => ctx.wiki.name.clone(),
            wiki_locale => ctx.wiki.default_locale.clone(),
            search_config => naw_core::search::regconfig_for(&ctx.wiki.default_locale),
            home_slug => home_slug,
            anonymous_create => rules.anonymous_create,
            anonymous_edit => rules.anonymous_edit,
            registered_create => rules.registered_create,
            registered_edit => rules.registered_edit,
            require_verified_email => rules.require_verified_email,
            raw_settings => serde_json::to_string_pretty(&ctx.wiki.settings)
                .unwrap_or_else(|_| "{}".to_string()),
        },
    )
}

#[derive(Debug, serde::Deserialize)]
pub struct WikiForm {
    name: String,
    default_locale: String,
    home_slug: String,
    // Unchecked boxes are absent from the post.
    #[serde(default)]
    anonymous_create: Option<String>,
    #[serde(default)]
    anonymous_edit: Option<String>,
    #[serde(default)]
    registered_create: Option<String>,
    #[serde(default)]
    registered_edit: Option<String>,
    #[serde(default)]
    require_verified_email: Option<String>,
}

/// POST /admin/wiki
#[instrument(skip(state, user, headers, form))]
pub async fn save_wiki_settings(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<WikiForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.can(Capability::WikiSettings) {
        return Ok((StatusCode::FORBIDDEN, "not allowed").into_response());
    }
    let name = form.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            "name: 1 to 120 characters",
        )
            .into_response());
    }
    let locale = form.default_locale.trim().to_lowercase();
    if !locale_is_sane(&locale) {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            "locale: a BCP 47 tag such as en or ru-RU",
        )
            .into_response());
    }
    let home_slug = form.home_slug.trim().to_lowercase();
    if !pages::slug_is_valid(&home_slug) {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            "home page: lowercase letters, digits and dashes",
        )
            .into_response());
    }

    // Merge: `aliases` and `default` live in the same object.
    let mut settings = ctx.wiki.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    let object = settings.as_object_mut().expect("just ensured an object");
    object.insert("home_slug".into(), json!(home_slug));
    object.insert(
        "permissions".into(),
        json!({
            "anonymous_create": form.anonymous_create.is_some(),
            "anonymous_edit": form.anonymous_edit.is_some(),
            "registered_create": form.registered_create.is_some(),
            "registered_edit": form.registered_edit.is_some(),
            "require_verified_email": form.require_verified_email.is_some(),
        }),
    );

    let locale_changed = locale != ctx.wiki.default_locale;
    sqlx::query!(
        "UPDATE wikis SET name = $2, default_locale = $3, settings = $4 WHERE id = $1",
        ctx.wiki.id,
        name,
        locale,
        settings
    )
    .execute(&state.db)
    .await?;

    // The stemmer is baked into every tsvector, so a locale change reindexes.
    let reindexed = if locale_changed {
        crate::indexing::reindex(&state.db, Some(ctx.wiki.id)).await?
    } else {
        0
    };

    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "wiki.settings",
            entity_type: "wiki",
            entity_id: Some(ctx.wiki.id),
            meta: json!({
                "locale": locale,
                "locale_changed": locale_changed,
                "reindexed": reindexed,
                "home_slug": home_slug,
            }),
        },
    )
    .await?;
    Ok(pages::see_other("/admin/wiki"))
}

/// A permissive BCP 47 shape check: letters, then dash-separated subtags.
fn locale_is_sane(locale: &str) -> bool {
    if locale.is_empty() || locale.len() > 20 {
        return false;
    }
    let mut parts = locale.split('-');
    let Some(primary) = parts.next() else {
        return false;
    };
    if primary.len() < 2 || primary.len() > 8 || !primary.chars().all(|c| c.is_ascii_lowercase()) {
        return false;
    }
    parts.all(|part| {
        !part.is_empty() && part.len() <= 8 && part.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

#[derive(Debug, serde::Deserialize)]
pub struct ReindexForm {
    #[serde(default)]
    confirm: Option<String>,
}

/// POST /admin/reindex, for an index that drifted.
#[instrument(skip(state, user, headers, form))]
pub async fn reindex(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<ReindexForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.can(Capability::WikiSettings) {
        return Ok((StatusCode::FORBIDDEN, "not allowed").into_response());
    }
    if form.confirm.is_none() {
        return Ok(pages::see_other("/admin/wiki"));
    }
    let count = crate::indexing::reindex(&state.db, Some(ctx.wiki.id)).await?;
    audit::record_or_log(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "search.reindex",
            entity_type: "wiki",
            entity_id: Some(ctx.wiki.id),
            meta: json!({ "pages": count }),
        },
    )
    .await;
    Ok(pages::see_other("/admin/wiki"))
}

// ---------------------------------------------------------------------------
// Languages and skin
// ---------------------------------------------------------------------------

/// GET /admin/languages: installed packs, which this wiki offers, and the
/// skin's last load and last failed reload.
#[instrument(skip(state, user, headers))]
pub async fn languages(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.can(Capability::WikiSettings) {
        return Ok((StatusCode::FORBIDDEN, "not allowed").into_response());
    }
    let disabled = crate::resolve::disabled_languages(&ctx.wiki.settings);
    let packs: Vec<minijinja::Value> = ctx
        .skin
        .messages
        .report()
        .into_iter()
        .map(|pack| {
            let code = pack.meta.code.to_ascii_lowercase();
            let authors: Vec<minijinja::Value> = pack
                .meta
                .authors
                .iter()
                .map(|a| {
                    minijinja::context! {
                        name => a.name.clone(),
                        role => a.role.clone(),
                        url => a.url.clone(),
                    }
                })
                .collect();
            minijinja::context! {
                code => code.clone(),
                name => pack.meta.name.clone(),
                native_name => pack.meta.native_name.clone(),
                version => pack.meta.version.clone(),
                engine => pack.meta.engine.clone(),
                direction => pack.meta.direction.clone(),
                installed_enabled => pack.meta.enabled,
                offered => pack.meta.enabled && !disabled.contains(&code),
                is_fallback => code == naw_core::i18n::FALLBACK,
                is_content_language => code == ctx.wiki.default_locale,
                messages => pack.messages,
                missing => pack.missing,
                percent => pack.percent,
                authors => authors,
            }
        })
        .collect();
    render(
        &ctx,
        "languages",
        &ctx.t("admin.languages"),
        minijinja::context! {
            packs => packs,
            problems => ctx.skin.messages.problems().to_vec(),
            skin_loaded_at => ctx.skin.loaded_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            reload_error => state.skin.last_error(),
            reload_interval => state.config.reload_interval_secs,
        },
    )
}

/// POST /admin/languages. Stored as the languages not offered, so a pack
/// installed later is offered by default.
#[instrument(skip(state, user, headers, body))]
pub async fn save_languages(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.can(Capability::WikiSettings) {
        return Ok((StatusCode::FORBIDDEN, "not allowed").into_response());
    }
    // Repeated keys do not survive axum's `Form` into a Vec.
    let offered: Vec<String> = form_values(&body, "offer");
    let installed: Vec<String> = ctx
        .skin
        .messages
        .languages()
        .into_iter()
        .map(str::to_string)
        .collect();
    // The wiki's content language cannot be switched off.
    let disabled: Vec<String> = installed
        .iter()
        .filter(|code| !offered.contains(code) && **code != ctx.wiki.default_locale)
        .cloned()
        .collect();

    let mut settings = ctx.wiki.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    let object = settings.as_object_mut().expect("just ensured an object");
    let languages = object.entry("languages").or_insert_with(|| json!({}));
    if !languages.is_object() {
        *languages = json!({});
    }
    languages
        .as_object_mut()
        .expect("just ensured an object")
        .insert("disabled".into(), json!(disabled));

    sqlx::query!(
        "UPDATE wikis SET settings = $2 WHERE id = $1",
        ctx.wiki.id,
        settings
    )
    .execute(&state.db)
    .await?;
    audit::record(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "wiki.languages",
            entity_type: "wiki",
            entity_id: Some(ctx.wiki.id),
            meta: json!({ "disabled": disabled }),
        },
    )
    .await?;
    Ok(pages::see_other("/admin/languages"))
}

/// Every value of one key in an urlencoded body.
fn form_values(body: &[u8], key: &str) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(body) else {
        return Vec::new();
    };
    text.split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .collect()
}

fn percent_decode(value: &str) -> String {
    let spaced = value.replace('+', " ");
    let bytes = spaced.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(byte) = std::str::from_utf8(bytes.get(i + 1..i + 3).unwrap_or_default())
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// POST /admin/reload: reloads the skin and packs now, even when nothing
/// looks changed. A broken file keeps the running version.
#[instrument(skip(state, user, headers))]
pub async fn reload(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    if !ctx.actor.can(Capability::WikiSettings) {
        return Ok((StatusCode::FORBIDDEN, "not allowed").into_response());
    }
    let outcome = state.skin.reload(true);
    audit::record_or_log(
        &state.db,
        audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "skin.reload",
            entity_type: "wiki",
            entity_id: Some(ctx.wiki.id),
            meta: json!({ "ok": outcome.is_ok() }),
        },
    )
    .await;
    Ok(pages::see_other("/admin/languages"))
}

// ---------------------------------------------------------------------------
// Error pages
// ---------------------------------------------------------------------------

/// The variants a kind has wording for.
fn variants_of(kind: crate::errors::Kind) -> &'static [&'static str] {
    match kind {
        crate::errors::Kind::AuthFailed => &[
            "bad_request",
            "cancelled",
            "expired",
            "upstream",
            "closed",
            "suspended",
        ],
        _ => &[],
    }
}

/// GET /admin/errors: every error kind, and whether the skin ships its own.
#[instrument(skip(state, user, headers))]
pub async fn error_gallery(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    let kinds: Vec<minijinja::Value> = crate::errors::Kind::ALL
        .into_iter()
        .map(|kind| {
            minijinja::context! {
                id => kind.id(),
                status => kind.status().as_u16(),
                icon => kind.icon(),
                title => ctx.t(&format!("errors.{}.title", kind.id())),
                own_template => ctx.skin.env.get_template(&format!("errors/{}.html", kind.id())).is_ok(),
                variants => variants_of(kind),
            }
        })
        .collect();
    render(
        &ctx,
        "errors",
        &ctx.t("admin.errors"),
        minijinja::context! { kinds => kinds },
    )
}

#[derive(Debug, serde::Deserialize)]
pub struct PreviewQuery {
    #[serde(default)]
    variant: Option<String>,
}

/// GET /admin/errors/{kind}: one error page as a reader sees it, served as a
/// 200 since it is a preview.
#[instrument(skip(state, user, headers))]
pub async fn error_preview(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(kind): Path<String>,
    Query(query): Query<PreviewQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(gate(&state, &headers, user.as_ref()).await);
    let Some(kind) = crate::errors::Kind::parse(&kind) else {
        return Ok(crate::errors::not_found());
    };
    let overrides = query
        .variant
        .as_deref()
        .and_then(|v| variants_of(kind).iter().find(|known| **known == v).copied())
        .map(|v| crate::errors::Overrides::for_page(&state, &ctx, kind, v))
        .unwrap_or_default();
    Ok(crate::errors::render(
        &ctx,
        kind,
        StatusCode::OK,
        Some(&ctx.t("admin.error_sample_detail")),
        Some("preview0000"),
        &overrides,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_shape_checks_catch_the_obvious() {
        for ok in ["a@b.co", "vadim+calpha@filian.wiki"] {
            assert!(email_is_plausible(ok), "{ok}");
        }
        for bad in [
            "", "no-at", "@b.co", "a@", "a@b", "a@.b", "a@b.", "a b@c.d", "a@b@c.d",
        ] {
            assert!(!email_is_plausible(bad), "{bad}");
        }
    }

    #[test]
    fn repeated_form_keys_are_all_collected_and_decoded() {
        assert_eq!(
            form_values(b"offer=en&offer=ru&other=x", "offer"),
            vec!["en".to_string(), "ru".to_string()]
        );
        assert_eq!(
            form_values(b"offer=zh%2DHans", "offer"),
            vec!["zh-hans".to_string()]
        );
        assert!(form_values(b"", "offer").is_empty());
        assert!(form_values(b"offer=", "offer").is_empty());
        assert!(form_values(&[0xff, 0xfe], "offer").is_empty());
    }

    #[test]
    fn a_bad_escape_passes_through_instead_of_panicking() {
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%D1%80%D1%83"), "ру");
    }

    #[test]
    fn page_actions_parse_and_map_to_the_right_capability() {
        assert_eq!(PageAction::parse("lock"), Some(PageAction::Lock));
        assert_eq!(PageAction::parse("unlock"), Some(PageAction::Unlock));
        assert_eq!(PageAction::parse("archive"), Some(PageAction::Archive));
        assert_eq!(PageAction::parse("restore"), Some(PageAction::Restore));
        assert_eq!(PageAction::parse("delete"), None);
        assert_eq!(PageAction::parse(""), None);
        assert_eq!(PageAction::parse("LOCK"), None);

        assert_eq!(PageAction::Lock.capability(), Capability::PageLock);
        assert_eq!(PageAction::Archive.capability(), Capability::PageDelete);
    }

    #[test]
    fn plain_locales_pass_the_shape_check() {
        for good in ["en", "ru", "ru-ru", "en-gb", "zh-hans", "pt-br"] {
            assert!(locale_is_sane(good), "{good}");
        }
    }

    #[test]
    fn junk_never_reaches_the_locale_column() {
        for bad in [
            "",
            "e",
            "en_GB",
            "en-",
            "-en",
            "en--gb",
            "en gb",
            "en;drop",
            "russian'",
            "<script>",
            "toolongprimarytag",
            &"a".repeat(21),
        ] {
            assert!(!locale_is_sane(bad), "{bad:?} must be refused");
        }
    }
}
