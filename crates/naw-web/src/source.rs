//! What a reader gets on /{slug}/edit when they may not edit the page: why,
//! who may, what they can do instead, and the source to read and copy. After
//! Wikipedia's "View source".

use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::pages::{self, ENGINE_VERSION, FoundPage};
use crate::perm::{Capability, Sanction, WikiRole};
use crate::resolve::Ctx;

/// Protection changes shown under the source.
const LOG_SHOWN: i64 = 5;

/// One reason this reader may not edit, with an optional way past it.
fn reason(text: String, link: Option<(String, String)>) -> minijinja::Value {
    minijinja::context! {
        text => text,
        href => link.as_ref().map(|l| l.0.clone()),
        link_label => link.map(|l| l.1),
    }
}

/// A protection level as the page's menu names it.
fn level_label(ctx: &Ctx, level: Option<&str>) -> String {
    ctx.t(&format!("page.protect_{}", level.unwrap_or("none")))
}

/// Why this actor may not edit `found`, most basic first: an account that may
/// not edit anywhere hears that before it hears about this page's level.
fn reasons(ctx: &Ctx, path: &str, found: &FoundPage, template_uses: i64) -> Vec<minijinja::Value> {
    let actor = &ctx.actor;
    let mut out = Vec::new();
    match actor.sanction {
        Some(Sanction::Ban) => out.push(reason(ctx.t("source.why_banned"), None)),
        Some(Sanction::Mute) => out.push(reason(ctx.t("source.why_muted"), None)),
        None if !actor.can(Capability::PageEdit) => {
            if !actor.is_signed_in() {
                if actor.rules.registered_edit {
                    let next = format!(
                        "/login?next={}",
                        pages::urlencode(&ctx.link(&format!("/{path}/edit")))
                    );
                    out.push(reason(
                        ctx.t("source.why_sign_in"),
                        Some((next, ctx.t("nav.sign_in"))),
                    ));
                } else {
                    out.push(reason(ctx.t("source.why_closed"), None));
                }
            } else if actor.rules.require_verified_email && !actor.email_verified {
                out.push(reason(
                    ctx.t("source.why_verify_email"),
                    Some(("/settings".to_string(), ctx.t("settings.title"))),
                ));
            } else {
                out.push(reason(ctx.t("source.why_no_edit"), None));
            }
        }
        None => {}
    }
    if let Some(level) = found.protection {
        let role = actor.effective_role().unwrap_or(WikiRole::Registered);
        if role < level || !actor.is_signed_in() {
            if pages::split_path(path).0 == "template" && !found.locked {
                let level = level_label(ctx, Some(level.as_str()));
                out.push(reason(
                    ctx.tn_with("source.why_template", template_uses, &[("level", &level)]),
                    None,
                ));
            } else {
                out.push(reason(
                    ctx.t_with(
                        "source.why_protected",
                        &[("level", &level_label(ctx, Some(level.as_str())))],
                    ),
                    None,
                ));
            }
        }
    }
    if out.is_empty() {
        out.push(reason(ctx.t("source.why_no_edit"), None));
    }
    out
}

/// The view-source page, answered with 403: the edit itself was refused.
pub(crate) async fn page(
    state: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    path: &str,
    found: &FoundPage,
) -> Result<Response, AppError> {
    let (namespace, bare) = pages::split_path(path);
    let template_uses = if namespace == "template" {
        crate::templates::uses(state, ctx, bare).await?.0
    } else {
        0
    };
    let templates = sqlx::query_scalar!(
        "SELECT template_slug FROM template_uses WHERE page_id = $1 ORDER BY template_slug",
        found.id
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|slug| {
        minijinja::context! {
            name => slug.clone(),
            href => ctx.link(&format!("/{}{slug}", pages::TEMPLATE_PREFIX)),
        }
    })
    .collect::<Vec<_>>();
    // Protection set here, and the admin panel's lock and unlock.
    let log = sqlx::query!(
        r#"SELECT a.action, a.meta->>'was' AS was, a.meta->>'now' AS now, a.created_at,
                  (SELECT u.username FROM users u WHERE u.id = a.user_id) AS who
           FROM audit_log a
           WHERE a.wiki_id = $1 AND a.entity_id = $2
             AND a.action IN ('page.protect', 'page.lock', 'page.unlock')
           ORDER BY a.created_at DESC LIMIT $3"#,
        ctx.wiki.id,
        found.id,
        LOG_SHOWN
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|row| {
        let now = match row.action.as_str() {
            "page.lock" => Some("moderator".to_string()),
            "page.unlock" => None,
            _ => row.now,
        };
        minijinja::context! {
            who => row.who,
            day => ctx.day(row.created_at),
            was => row.was.as_deref().map(|l| level_label(ctx, Some(l))),
            now => level_label(ctx, now.as_deref()),
        }
    })
    .collect::<Vec<_>>();

    let lines = found.body_md.lines().count().max(1);
    let template = ctx
        .skin
        .env
        .get_template("source.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t_with("source.title", &[("page", &found.title)]),
                page_title => found.title.clone(),
                version => ENGINE_VERSION,
                slug => path,
                can_edit_this => false,
                reasons => reasons(ctx, path, found, template_uses),
                level => found.protection.map(|l| level_label(ctx, Some(l.as_str()))),
                source => found.body_md.clone(),
                rows => lines.clamp(8, 30),
                size => crate::files::human_size(ctx, found.body_md.len() as i64),
                lines => lines,
                templates => templates,
                log => log,
                can_suggest => ctx.actor.can(Capability::ReportSend),
                revision => found.revision_id.to_string(),
                page_href => ctx.link(&format!("/{path}")),
            }
        })
        .map_err(pages::template_error)?;
    let mut response = pages::html_response(html, headers);
    *response.status_mut() = StatusCode::FORBIDDEN;
    Ok(response)
}
