//! Reports from readers to the wiki's staff: a mistake in an article, a
//! complaint about a page or a person, or a change to a page the reader may
//! not edit. Readers send them from the page or the profile; moderators and up
//! work them from /admin/reports.
//!
//! Sending needs an account (`ReportSend`), so a report always has somebody
//! to answer, and is limited per person: a few in a burst, a cap on open ones,
//! and one open report per subject and kind.

use axum::extract::{Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::instrument;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages::{self, ENGINE_VERSION};
use crate::perm::Capability;
use crate::resolve::Ctx;

pub const KINDS: [&str; 3] = ["mistake", "complaint", "suggestion"];
pub const REASONS: [&str; 6] = [
    "spam",
    "vandalism",
    "abuse",
    "copyright",
    "privacy",
    "other",
];
pub const STATUSES: [&str; 3] = ["open", "resolved", "dismissed"];

const MESSAGE_MIN: usize = 3;
const MESSAGE_MAX: usize = 4000;
const RESPONSE_MAX: usize = 2000;
/// Reports one person may send in `BURST_MINUTES`.
const BURST_MAX: i64 = 5;
const BURST_MINUTES: i32 = 10;
/// Open reports one person may have on one wiki.
const OPEN_MAX: i64 = 20;
const PER_PAGE: i64 = 30;
/// A suggestion's text box starts with the page's text up to this size; a
/// bigger page starts empty, and the reader pastes what they changed.
const PREFILL_MAX: usize = 512 * 1024;

/// What a report is about.
enum Subject {
    Page {
        path: String,
        found: pages::FoundPage,
    },
    User {
        id: Uuid,
        username: String,
    },
}

impl Subject {
    /// The subject as stored: the page path, or `user:name`.
    fn key(&self) -> String {
        match self {
            Self::Page { path, .. } => path.clone(),
            Self::User { username, .. } => format!("user:{username}"),
        }
    }

    fn title(&self) -> String {
        match self {
            Self::Page { found, .. } => found.title.clone(),
            Self::User { username, .. } => username.clone(),
        }
    }

    fn href(&self, ctx: &Ctx) -> String {
        match self {
            Self::Page { path, .. } => ctx.link(&format!("/{path}")),
            Self::User { username, .. } => format!("/user/{username}"),
        }
    }

    /// Where the form posts.
    fn action(&self, ctx: &Ctx) -> String {
        format!("{}/report", self.href(ctx))
    }

    /// The kinds that make sense for it: a person is only complained about.
    fn kinds(&self) -> &'static [&'static str] {
        match self {
            Self::Page { .. } => &KINDS,
            Self::User { .. } => &KINDS[1..2],
        }
    }
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct FormQuery {
    kind: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct ReportForm {
    kind: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    proposed: String,
    #[serde(default)]
    revision: String,
}

/// The values the form shows again after a refusal.
struct Filled<'a> {
    kind: &'a str,
    reason: &'a str,
    message: &'a str,
    proposed: Option<&'a str>,
    revision: String,
}

async fn page_subject(
    state: &AppState,
    ctx: &Ctx,
    slug: &str,
) -> Result<Option<Subject>, AppError> {
    let slug = slug.trim().to_lowercase();
    if !pages::slug_is_valid(&slug) {
        return Ok(None);
    }
    Ok(
        pages::find_page(&state.db, ctx.wiki.id, &slug, &ctx.content_locale)
            .await?
            .map(|found| Subject::Page { path: slug, found }),
    )
}

async fn user_subject(state: &AppState, name: &str) -> Result<Option<Subject>, AppError> {
    Ok(sqlx::query!(
        "SELECT id, username FROM users WHERE lower(username) = $1",
        name.trim().to_lowercase()
    )
    .fetch_optional(&state.db)
    .await?
    .map(|row| Subject::User {
        id: row.id,
        username: row.username,
    }))
}

/// A guest is sent to sign in, then back to the form; anybody else without
/// the capability is told no.
#[allow(clippy::result_large_err)]
fn gate(ctx: &Ctx, subject: &Subject) -> Result<(), Response> {
    if ctx.actor.can(Capability::ReportSend) {
        return Ok(());
    }
    if !ctx.actor.is_signed_in() {
        let next = format!("/login?next={}", pages::urlencode(&subject.action(ctx)));
        return Err(pages::see_other(&next));
    }
    Err(pages::notice(
        ctx,
        StatusCode::FORBIDDEN,
        &ctx.t("error.not_allowed"),
        &ctx.t("report.refused"),
        &subject.href(ctx),
        &ctx.t("report.back"),
    )
    .unwrap_or_else(IntoResponse::into_response))
}

fn render_form(
    ctx: &Ctx,
    subject: &Subject,
    filled: &Filled<'_>,
    problem: Option<String>,
    status: StatusCode,
) -> Result<Response, AppError> {
    let kinds = subject
        .kinds()
        .iter()
        .map(|id| {
            minijinja::context! {
                id => *id,
                label => ctx.t(&format!("report.kind_{id}")),
                note => ctx.t(&format!("report.kind_{id}_note")),
                current => *id == filled.kind,
            }
        })
        .collect::<Vec<_>>();
    let reasons = REASONS
        .iter()
        .map(|id| {
            minijinja::context! {
                id => *id,
                label => ctx.t(&format!("report.reason_{id}")),
                current => *id == filled.reason,
            }
        })
        .collect::<Vec<_>>();
    let is_page = matches!(subject, Subject::Page { .. });
    let template = ctx
        .skin
        .env
        .get_template("report.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t_with("report.title", &[("subject", &subject.title())]),
                version => ENGINE_VERSION,
                subject_title => subject.title(),
                subject_href => subject.href(ctx),
                about_user => !is_page,
                action => subject.action(ctx),
                kinds => kinds,
                reasons => reasons,
                message => filled.message,
                proposed => filled.proposed,
                revision => filled.revision.clone(),
                problem => problem,
                message_max => MESSAGE_MAX,
            }
        })
        .map_err(pages::template_error)?;
    let mut response = pages::html_response(html, &HeaderMap::new());
    *response.status_mut() = status;
    Ok(response)
}

fn kind_or_default(subject: &Subject, raw: Option<&str>) -> &'static str {
    let kinds = subject.kinds();
    raw.and_then(|k| kinds.iter().find(|id| **id == k).copied())
        .unwrap_or(kinds[0])
}

fn show_form(ctx: &Ctx, subject: Subject, query: &FormQuery) -> Result<Response, AppError> {
    if let Err(response) = gate(ctx, &subject) {
        return Ok(response);
    }
    let kind = kind_or_default(&subject, query.kind.as_deref());
    // A suggestion starts from the text as it stands, and remembers which
    // revision that was.
    let (proposed, revision) = match &subject {
        Subject::Page { found, .. } => (
            Some(if found.body_md.len() <= PREFILL_MAX {
                found.body_md.as_str()
            } else {
                ""
            }),
            found.revision_id.to_string(),
        ),
        Subject::User { .. } => (None, String::new()),
    };
    render_form(
        ctx,
        &subject,
        &Filled {
            kind,
            reason: "",
            message: "",
            proposed,
            revision,
        },
        None,
        StatusCode::OK,
    )
}

async fn send(
    state: &AppState,
    ctx: &Ctx,
    subject: Subject,
    form: &ReportForm,
) -> Result<Response, AppError> {
    if let Err(response) = gate(ctx, &subject) {
        return Ok(response);
    }
    let Some(me) = ctx.actor.user_id else {
        return Ok(crate::errors::not_found());
    };
    let kind = kind_or_default(&subject, Some(&form.kind));
    let message = form.message.trim();
    let reason = (kind == "complaint")
        .then(|| REASONS.iter().find(|r| **r == form.reason).copied())
        .flatten();
    // A suggestion may be words alone; an empty text box proposes no text.
    let proposed = (kind == "suggestion"
        && matches!(subject, Subject::Page { .. })
        && !form.proposed.trim().is_empty())
    .then(|| form.proposed.replace("\r\n", "\n"));
    let refill = |problem: String, status: StatusCode| {
        render_form(
            ctx,
            &subject,
            &Filled {
                kind,
                reason: reason.unwrap_or(""),
                message,
                proposed: proposed.as_deref(),
                revision: form.revision.clone(),
            },
            Some(problem),
            status,
        )
    };

    let chars = message.chars().count();
    if !(MESSAGE_MIN..=MESSAGE_MAX).contains(&chars) {
        return refill(
            ctx.t_with(
                "report.message_length",
                &[("max", &MESSAGE_MAX.to_string())],
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    if kind == "complaint" && reason.is_none() {
        return refill(
            ctx.t("report.reason_needed"),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    if let (Some(text), Subject::Page { found, .. }) = (&proposed, &subject) {
        if text.len() > pages::BODY_MAX {
            return refill(ctx.t("report.too_long"), StatusCode::PAYLOAD_TOO_LARGE);
        }
        if text.trim() == found.body_md.trim() {
            return refill(
                ctx.t("report.nothing_changed"),
                StatusCode::UNPROCESSABLE_ENTITY,
            );
        }
    }

    let key = subject.key();
    let limits = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM reports WHERE reporter_id = $1
                AND created_at > now() - make_interval(mins => $3)) AS "burst!",
             (SELECT count(*) FROM reports WHERE reporter_id = $1 AND wiki_id = $2
                AND status = 'open') AS "open!",
             (SELECT count(*) FROM reports WHERE reporter_id = $1 AND wiki_id = $2
                AND status = 'open' AND subject = $4 AND kind = $5) AS "same!""#,
        me,
        ctx.wiki.id,
        BURST_MINUTES,
        key,
        kind
    )
    .fetch_one(&state.db)
    .await?;
    if limits.same > 0 {
        return refill(ctx.t("report.already_open"), StatusCode::CONFLICT);
    }
    if limits.burst >= BURST_MAX || limits.open >= OPEN_MAX {
        return refill(ctx.t("report.slow_down"), StatusCode::TOO_MANY_REQUESTS);
    }

    let (page_id, revision_id) = match &subject {
        Subject::Page { found, .. } => (
            Some(found.id),
            pages::parse_uuid(&form.revision).or(Some(found.revision_id)),
        ),
        Subject::User { .. } => (None, None),
    };
    let id = Uuid::new_v4();
    // The revision must be this page's: the form field is not trusted.
    sqlx::query!(
        "INSERT INTO reports (id, wiki_id, kind, reason, subject, page_id, locale, revision_id,
                              message, proposed, reporter_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7,
                 (SELECT r.id FROM revisions r WHERE r.id = $8 AND r.page_id = $6),
                 $9, $10, $11)",
        id,
        ctx.wiki.id,
        kind,
        reason,
        key,
        page_id,
        page_id.map(|_| ctx.content_locale.clone()),
        revision_id,
        message,
        proposed,
        me
    )
    .execute(&state.db)
    .await?;
    let about_user = match &subject {
        Subject::User { id, .. } => Some(*id),
        Subject::Page { .. } => None,
    };
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: Some(me),
            action: "report.send",
            entity_type: "report",
            entity_id: Some(id),
            meta: json!({
                "kind": kind,
                "reason": reason,
                "subject": key,
                "user": about_user,
            }),
        },
    )
    .await;
    pages::notice_ok(
        ctx,
        &ctx.t("report.sent_title"),
        &ctx.t("report.sent_body"),
        &subject.href(ctx),
        &ctx.t("report.back"),
    )
}

/// GET /{slug}/report
#[instrument(skip(state, user, headers))]
pub async fn page_form(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    Query(query): Query<FormQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = page_subject(&state, &ctx, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    show_form(&ctx, subject, &query)
}

/// POST /{slug}/report
#[instrument(skip(state, user, form, headers))]
pub async fn page_send(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<ReportForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = page_subject(&state, &ctx, &slug).await? else {
        return Ok(crate::errors::not_found());
    };
    send(&state, &ctx, subject, &form).await
}

/// GET /user/{name}/report
#[instrument(skip(state, user, headers))]
pub async fn user_form(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = user_subject(&state, &name).await? else {
        return Ok(crate::errors::not_found());
    };
    show_form(&ctx, subject, &FormQuery::default())
}

/// POST /user/{name}/report
#[instrument(skip(state, user, form, headers))]
pub async fn user_send(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(form): Form<ReportForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let Some(subject) = user_subject(&state, &name).await? else {
        return Ok(crate::errors::not_found());
    };
    send(&state, &ctx, subject, &form).await
}

// ---------------------------------------------------------------------------
// The queue
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct QueueQuery {
    status: Option<String>,
    kind: Option<String>,
    page: Option<i64>,
}

/// Open reports on this wiki, for the admin menu's badge.
pub(crate) async fn open_count(db: &sqlx::PgPool, wiki_id: Uuid) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM reports WHERE wiki_id = $1 AND status = 'open'"#,
        wiki_id
    )
    .fetch_one(db)
    .await?)
}

/// A report's link to what it is about.
/// How many of a reader's own reports the settings page lists.
const MINE_SHOWN: i64 = 20;

/// A reader's own reports in this wiki, newest first, with the staff's answer:
/// what they sent is theirs to follow up on.
pub(crate) async fn mine(
    state: &AppState,
    ctx: &Ctx,
    user_id: Uuid,
) -> Result<Vec<minijinja::Value>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT r.kind, r.reason, r.subject, r.locale, r.message, r.status,
                  r.created_at, r.handled_at, r.response,
                  (SELECT p.title FROM pages p WHERE p.id = r.page_id AND p.deleted_at IS NULL) AS page_title
           FROM reports r
           WHERE r.wiki_id = $1 AND r.reporter_id = $2
           ORDER BY r.created_at DESC
           LIMIT $3"#,
        ctx.wiki.id,
        user_id,
        MINE_SHOWN
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            minijinja::context! {
                kind => r.kind.clone(),
                kind_label => ctx.t(&format!("report.short_{}", r.kind)),
                reason => r.reason.as_deref().map(|x| ctx.t(&format!("report.reason_{x}"))),
                subject_title => r.page_title.unwrap_or_else(|| r.subject.clone()),
                subject_href => subject_href(ctx, &r.subject, r.locale.as_deref()),
                message => excerpt(&r.message),
                status => r.status.clone(),
                status_label => ctx.t(&format!("report.status_{}", r.status)),
                at => ctx.day(r.created_at),
                handled_at => r.handled_at.map(|t| ctx.day(t)),
                response => r.response.filter(|a| !a.trim().is_empty()),
            }
        })
        .collect())
}

fn subject_href(ctx: &Ctx, subject: &str, locale: Option<&str>) -> String {
    match subject.strip_prefix("user:") {
        Some(name) => format!("/user/{name}"),
        None => match locale {
            Some(locale) => ctx.link_for(locale, &format!("/{subject}")),
            None => ctx.link(&format!("/{subject}")),
        },
    }
}

/// GET /admin/reports
#[instrument(skip(state, user, headers))]
pub async fn queue(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<QueueQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::ReportHandle).await
    );
    let status = query
        .status
        .as_deref()
        .filter(|s| *s == "all" || STATUSES.contains(s))
        .unwrap_or("open");
    let kind = query.kind.as_deref().filter(|k| KINDS.contains(k));
    let page_no = query.page.unwrap_or(1).max(1);
    let offset = (page_no - 1) * PER_PAGE;
    let status_filter = (status != "all").then_some(status);

    let rows = sqlx::query!(
        r#"SELECT r.id, r.kind, r.reason, r.subject, r.locale, r.message, r.status,
                  r.created_at, r.handled_at, r.response, (r.proposed IS NOT NULL) AS "has_proposed!",
                  (SELECT u.username FROM users u WHERE u.id = r.reporter_id) AS reporter,
                  (SELECT u.username FROM users u WHERE u.id = r.handled_by) AS handler,
                  (SELECT p.title FROM pages p WHERE p.id = r.page_id AND p.deleted_at IS NULL) AS page_title
           FROM reports r
           WHERE r.wiki_id = $1
             AND ($2::text IS NULL OR r.status = $2)
             AND ($3::text IS NULL OR r.kind = $3)
           ORDER BY r.created_at DESC
           LIMIT $4 OFFSET $5"#,
        ctx.wiki.id,
        status_filter,
        kind,
        PER_PAGE,
        offset
    )
    .fetch_all(&state.db)
    .await?;
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM reports r
           WHERE r.wiki_id = $1
             AND ($2::text IS NULL OR r.status = $2)
             AND ($3::text IS NULL OR r.kind = $3)"#,
        ctx.wiki.id,
        status_filter,
        kind
    )
    .fetch_one(&state.db)
    .await?;

    let reports = rows
        .into_iter()
        .map(|r| {
            minijinja::context! {
                id => r.id.to_string(),
                kind => r.kind.clone(),
                kind_label => ctx.t(&format!("report.short_{}", r.kind)),
                reason => r.reason.as_deref().map(|x| ctx.t(&format!("report.reason_{x}"))),
                subject => r.subject.clone(),
                subject_title => r.page_title.unwrap_or_else(|| r.subject.clone()),
                subject_href => subject_href(&ctx, &r.subject, r.locale.as_deref()),
                message => excerpt(&r.message),
                status => r.status.clone(),
                status_label => ctx.t(&format!("report.status_{}", r.status)),
                reporter => r.reporter,
                handler => r.handler,
                at => ctx.day(r.created_at),
                handled_at => r.handled_at.map(|t| ctx.day(t)),
                response => r.response,
                has_proposed => r.has_proposed,
            }
        })
        .collect::<Vec<_>>();
    let filter = |id: &str, label: String, current: bool| {
        minijinja::context! { id => id.to_string(), label => label, current => current }
    };
    let status_filters = ["open", "resolved", "dismissed", "all"]
        .iter()
        .map(|s| filter(s, ctx.t(&format!("report.status_{s}")), *s == status))
        .collect::<Vec<_>>();
    let kind_filters = std::iter::once(filter("", ctx.t("report.kind_all"), kind.is_none()))
        .chain(
            KINDS
                .iter()
                .map(|k| filter(k, ctx.t(&format!("report.short_{k}")), Some(*k) == kind)),
        )
        .collect::<Vec<_>>();
    let open = open_count(&state.db, ctx.wiki.id).await?;
    crate::admin::render(
        &ctx,
        "reports",
        &ctx.t("admin.reports"),
        minijinja::context! {
            reports => reports,
            total => total,
            status => status,
            kind => kind.unwrap_or(""),
            status_filters => status_filters,
            kind_filters => kind_filters,
            open_reports => open,
            page_no => page_no,
            has_prev => page_no > 1,
            has_next => offset + PER_PAGE < total,
            prev_page => page_no - 1,
            next_page => page_no + 1,
        },
    )
}

/// GET /admin/reports/{id}: one report, and for a suggestion what it changes.
#[instrument(skip(state, user, headers))]
pub async fn show(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::ReportHandle).await
    );
    let Some(id) = pages::parse_uuid(&id) else {
        return Ok(crate::errors::not_found());
    };
    let Some(r) = sqlx::query!(
        r#"SELECT r.id, r.kind, r.reason, r.subject, r.locale, r.message, r.status, r.proposed,
                  r.created_at, r.handled_at, r.response,
                  (SELECT u.username FROM users u WHERE u.id = r.reporter_id) AS reporter,
                  (SELECT u.username FROM users u WHERE u.id = r.handled_by) AS handler,
                  (SELECT v.body_md FROM revisions v WHERE v.id = r.revision_id) AS base_body,
                  (SELECT p.title FROM pages p WHERE p.id = r.page_id AND p.deleted_at IS NULL) AS page_title,
                  (SELECT p.current_revision_id FROM pages p WHERE p.id = r.page_id) AS current_revision,
                  r.revision_id
           FROM reports r WHERE r.id = $1 AND r.wiki_id = $2"#,
        id,
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    let diff = match (&r.proposed, &r.base_body) {
        (Some(proposed), Some(base)) => {
            let computed = crate::diff::diff_bodies(base, proposed);
            Some(minijinja::context! {
                rows => computed.rows.iter().map(crate::history::row_value).collect::<Vec<_>>(),
                added => computed.added,
                removed => computed.removed,
                truncated => computed.truncated,
                // The page moved on since the reader wrote this.
                stale => r.current_revision.is_some() && r.current_revision != r.revision_id,
            })
        }
        _ => None,
    };
    let subject_href = subject_href(&ctx, &r.subject, r.locale.as_deref());
    let edit_href =
        (r.proposed.is_some() && r.page_title.is_some()).then(|| format!("{subject_href}/edit"));
    crate::admin::render(
        &ctx,
        "report",
        &ctx.t_with(
            "report.detail_title",
            &[("kind", &ctx.t(&format!("report.short_{}", r.kind)))],
        ),
        minijinja::context! {
            report => minijinja::context! {
                id => r.id.to_string(),
                kind => r.kind.clone(),
                reason => r.reason.as_deref().map(|x| ctx.t(&format!("report.reason_{x}"))),
                subject_title => r.page_title.clone().unwrap_or_else(|| r.subject.clone()),
                subject_href => subject_href,
                message => r.message,
                status => r.status.clone(),
                status_label => ctx.t(&format!("report.status_{}", r.status)),
                reporter => r.reporter,
                handler => r.handler,
                at => ctx.day(r.created_at),
                handled_at => r.handled_at.map(|t| ctx.day(t)),
                response => r.response,
                proposed => r.proposed,
                edit_href => edit_href,
            },
            diff => diff,
            open_reports => open_count(&state.db, ctx.wiki.id).await?,
        },
    )
}

#[derive(serde::Deserialize)]
pub struct StatusForm {
    status: String,
    #[serde(default)]
    response: String,
}

/// POST /admin/reports/{id}/status: resolve, dismiss or reopen, with an answer.
#[instrument(skip(state, user, form, headers))]
pub async fn set_status(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<StatusForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::ReportHandle).await
    );
    let Some(id) = pages::parse_uuid(&id) else {
        return Ok(crate::errors::not_found());
    };
    if !STATUSES.contains(&form.status.as_str()) {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "unknown status").into_response());
    }
    let response = form.response.trim();
    if response.chars().count() > RESPONSE_MAX {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "the answer is too long").into_response());
    }
    let reopened = form.status == "open";
    // `wiki_id` in the WHERE clause is the tenancy check.
    let Some(row) = sqlx::query!(
        r#"WITH old AS (
             SELECT id, status FROM reports WHERE id = $1 AND wiki_id = $2 FOR UPDATE
           )
           UPDATE reports r SET status = $3,
                  handled_by = CASE WHEN $4 THEN NULL ELSE $5::uuid END,
                  handled_at = CASE WHEN $4 THEN NULL ELSE now() END,
                  response = CASE WHEN $6 = '' THEN r.response ELSE $6 END
           FROM old WHERE r.id = old.id
           RETURNING old.status AS was, r.kind"#,
        id,
        ctx.wiki.id,
        form.status,
        reopened,
        ctx.actor.user_id,
        response
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "report.status",
            entity_type: "report",
            entity_id: Some(id),
            meta: json!({ "kind": row.kind, "was": row.was, "now": form.status }),
        },
    )
    .await;
    Ok(pages::see_other(&format!("/admin/reports/{id}")))
}

/// Characters of a message the queue shows before the card is opened.
const EXCERPT_CHARS: usize = 280;

/// The start of a message, cut at a character, with an ellipsis when cut.
fn excerpt(message: &str) -> String {
    match message.char_indices().nth(EXCERPT_CHARS) {
        Some((at, _)) => format!("{}…", message[..at].trim_end()),
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_excerpt_cuts_at_a_character() {
        assert_eq!(excerpt("short"), "short");
        let long = "Ф".repeat(EXCERPT_CHARS + 5);
        let cut = excerpt(&long);
        assert_eq!(cut.chars().count(), EXCERPT_CHARS + 1);
        assert!(cut.ends_with('…'));
    }
}
