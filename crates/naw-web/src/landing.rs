//! The front page: a landing, not only an article. It says what the wiki is
//! for, offers search and the main sections, shows what changed lately and
//! how to help, and then the home article, which stays an ordinary page any
//! editor can change. The article's first paragraph is the landing's lede.

use axum::http::HeaderMap;
use axum::response::Response;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::pages::{self, ENGINE_VERSION};
use crate::resolve::Ctx;

/// Recently edited articles shown on the landing.
const RECENT_SHOWN: i64 = 6;

/// The home article split for the landing: its heading and first paragraph
/// go to the hero, the rest stays below.
struct Split {
    lede: Option<String>,
    rest: String,
}

/// Takes the first `<h1>` and the first paragraph out of rendered HTML.
fn split_lede(html: &str) -> Split {
    let mut rest = html.to_string();
    if let Some(open) = rest.find("<h1")
        && let Some(close) = rest[open..].find("</h1>")
    {
        rest.replace_range(open..open + close + "</h1>".len(), "");
    }
    let lede = match rest.find("<p>") {
        Some(open) => match rest[open..].find("</p>") {
            Some(close) => {
                let inner = rest[open + 3..open + close].to_string();
                rest.replace_range(open..open + close + "</p>".len(), "");
                Some(inner)
            }
            None => None,
        },
        None => None,
    };
    Split {
        lede: lede.filter(|l| !l.trim().is_empty()),
        rest,
    }
}

/// The home article's slug, from the wiki's settings.
pub(crate) fn home_slug(ctx: &Ctx) -> String {
    ctx.wiki
        .settings
        .get("home_slug")
        .and_then(|v| v.as_str())
        .filter(|s| pages::slug_is_valid(s))
        .unwrap_or("home")
        .to_string()
}

/// GET / and the home article's own address, which older links and browsers
/// that remember the old redirect still open.
pub(crate) async fn page(
    state: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let home_slug = home_slug(ctx);
    let home = pages::find_page(&state.db, ctx.wiki.id, &home_slug, &ctx.content_locale).await?;
    let (split, can_edit_home) = match &home {
        Some(page) => {
            let (html, _) = pages::cached_body(state, ctx, &home_slug, &page.body_md).await?;
            (split_lede(&html), ctx.actor.can_edit_page(page.protection))
        }
        None => (
            Split {
                lede: None,
                rest: String::new(),
            },
            false,
        ),
    };

    let recent = sqlx::query!(
        r#"SELECT title AS "title!", slug AS "slug!", at AS "at!" FROM (
             SELECT DISTINCT ON (p.id) p.title, p.slug, r.created_at AS at
             FROM revisions r JOIN pages p ON p.id = r.page_id
             WHERE p.wiki_id = $1 AND p.namespace = 'main' AND p.deleted_at IS NULL
               AND COALESCE(p.locale, '') = $2
             ORDER BY p.id, r.created_at DESC
           ) latest
           ORDER BY at DESC LIMIT $3"#,
        ctx.wiki.id,
        ctx.content_locale,
        RECENT_SHOWN
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|row| {
        minijinja::context! {
            title => row.title,
            href => ctx.link(&format!("/{}", row.slug)),
            day => ctx.day(row.at),
        }
    })
    .collect::<Vec<_>>();

    let counts = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL) AS "articles!",
             (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id WHERE p.wiki_id = $1) AS "edits!",
             (SELECT count(*) FROM media WHERE wiki_id = $1) AS "files!""#,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?;

    let template = ctx
        .skin
        .env
        .get_template("landing.html")
        .map_err(pages::template_error)?;
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.wiki.name.clone(),
                version => ENGINE_VERSION,
                lede => split.lede,
                home_body => (!split.rest.trim().is_empty()).then_some(split.rest),
                home_href => ctx.link(&format!("/{home_slug}")),
                home_edit => can_edit_home.then(|| ctx.link(&format!("/{home_slug}/edit"))),
                home_history => ctx.link(&format!("/{home_slug}/history")),
                // The menu offers the home article's tools like any page's.
                slug => home.is_some().then(|| home_slug.clone()),
                can_edit_this => can_edit_home,
                recent => recent,
                articles => counts.articles,
                edits => counts.edits,
                files => counts.files,
            }
        })
        .map_err(pages::template_error)?;
    Ok(pages::html_response(html, headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_heading_and_first_paragraph_become_the_lede() {
        let split = split_lede(
            "<h1 id=\"w\">Welcome</h1>\n<p>The <a href=\"/x\">fan</a> wiki.</p>\n<p>More.</p>",
        );
        assert_eq!(
            split.lede.as_deref(),
            Some("The <a href=\"/x\">fan</a> wiki.")
        );
        assert_eq!(split.rest.trim(), "<p>More.</p>");
    }

    #[test]
    fn a_body_without_paragraphs_has_no_lede() {
        let split = split_lede("<ul><li>x</li></ul>");
        assert_eq!(split.lede, None);
        assert_eq!(split.rest, "<ul><li>x</li></ul>");
    }
}
