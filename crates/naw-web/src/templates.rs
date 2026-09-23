//! Templates on the wiki side: loading the `Template:` pages a body calls,
//! expanding them, and remembering which pages use which template.
//!
//! The render cache is keyed by the hash of the expanded text, so an edit to
//! a template changes the key of every page that uses it. Those pages render
//! fresh on their next view and nothing has to be purged.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;
use naw_markdown::transclude;

use crate::resolve::Ctx;

/// Distinct templates one page may pull in.
const TEMPLATES_MAX: usize = 200;

/// Pages listed on a template's own page as using it.
pub(crate) const USES_SHOWN: i64 = 50;

pub(crate) struct Expanded {
    pub text: String,
    /// Slugs of the templates the text used.
    pub used: Vec<String>,
}

/// What a page's source becomes before Markdown: a template page shows its
/// documentation with parameters at their defaults, any page has its calls
/// expanded.
pub(crate) async fn expand(
    state: &AppState,
    ctx: &Ctx,
    path: &str,
    body: &str,
) -> Result<Expanded, AppError> {
    let source = if crate::pages::split_path(path).0 == "template" {
        transclude::template_view(body)
    } else {
        body.to_string()
    };
    if !source.contains("{{") {
        return Ok(Expanded {
            text: source,
            used: Vec::new(),
        });
    }
    let notes = transclude::Notes {
        missing: ctx.t_with("template.missing", &[("name", "{name}")]),
        looped: ctx.t_with("template.looped", &[("name", "{name}")]),
        limit: ctx.t("template.limit"),
    };
    let mut loaded: HashMap<String, String> = HashMap::new();
    let mut tried: HashSet<String> = HashSet::new();
    // Each round loads what the templates of the last round call.
    for _ in 0..transclude::DEPTH_MAX {
        let run = transclude::expand(&source, &loaded, &notes);
        let room = TEMPLATES_MAX.saturating_sub(tried.len());
        let wanted: Vec<String> = run
            .missing
            .into_iter()
            .filter(|slug| !tried.contains(slug))
            .take(room)
            .collect();
        if wanted.is_empty() {
            return Ok(Expanded {
                text: run.text,
                used: run.used.into_iter().collect(),
            });
        }
        tried.extend(wanted.iter().cloned());
        loaded.extend(load(state, ctx, &wanted).await?);
    }
    let run = transclude::expand(&source, &loaded, &notes);
    Ok(Expanded {
        text: run.text,
        used: run.used.into_iter().collect(),
    })
}

/// The live source of each template, in the reader's language when there is
/// one, else the wiki's own, else any.
async fn load(
    state: &AppState,
    ctx: &Ctx,
    slugs: &[String],
) -> Result<HashMap<String, String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (p.slug) p.slug AS "slug!", r.body_md AS "body!"
           FROM pages p JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.slug = ANY($2)
             AND p.deleted_at IS NULL
           ORDER BY p.slug, (COALESCE(p.locale, '') = $3) DESC, (COALESCE(p.locale, '') = $4) DESC"#,
        ctx.wiki.id,
        slugs,
        ctx.content_locale,
        ctx.wiki.default_locale
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(|row| (row.slug, row.body)).collect())
}

/// Replaces the list of templates `page_id` uses. A failure is logged: the
/// list feeds "used on" and is rebuilt by the next save.
pub(crate) async fn record_uses(state: &AppState, wiki_id: Uuid, page_id: Uuid, used: &[String]) {
    let result: Result<(), sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        sqlx::query!("DELETE FROM template_uses WHERE page_id = $1", page_id)
            .execute(&mut *tx)
            .await?;
        if !used.is_empty() {
            sqlx::query!(
                "INSERT INTO template_uses (page_id, wiki_id, template_slug)
                 SELECT $1, $2, unnest($3::text[]) ON CONFLICT DO NOTHING",
                page_id,
                wiki_id,
                used
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
    .await;
    if let Err(err) = result {
        tracing::warn!(error = %err, %page_id, "could not record the templates a page uses");
    }
}

/// One page that uses a template.
pub(crate) struct Use {
    pub title: String,
    pub href: String,
}

/// How many pages use `slug`, and the first of them by title.
pub(crate) async fn uses(
    state: &AppState,
    ctx: &Ctx,
    slug: &str,
) -> Result<(i64, Vec<Use>), AppError> {
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM template_uses u JOIN pages p ON p.id = u.page_id
           WHERE u.wiki_id = $1 AND u.template_slug = $2 AND p.deleted_at IS NULL"#,
        ctx.wiki.id,
        slug
    )
    .fetch_one(&state.db)
    .await?;
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, p.namespace::text AS "namespace!"
           FROM template_uses u JOIN pages p ON p.id = u.page_id
           WHERE u.wiki_id = $1 AND u.template_slug = $2 AND p.deleted_at IS NULL
           ORDER BY p.title LIMIT $3"#,
        ctx.wiki.id,
        slug,
        USES_SHOWN
    )
    .fetch_all(&state.db)
    .await?;
    let shown = rows
        .into_iter()
        .map(|row| Use {
            href: match row.namespace.as_str() {
                "user" => format!("/user/{}", row.slug),
                "template" => ctx.link(&format!("/template:{}", row.slug)),
                _ => ctx.link(&format!("/{}", row.slug)),
            },
            title: row.title,
        })
        .collect();
    Ok((total, shown))
}

/// Templates a new page can start from: those whose source begins with
/// `<!-- starter: Label -->`. The comment never renders.
pub(crate) async fn starters(
    state: &AppState,
    ctx: &Ctx,
) -> Result<Vec<(String, String)>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (p.slug) p.slug AS "slug!", left(r.body_md, 300) AS "head!"
           FROM pages p JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.deleted_at IS NULL
             AND r.body_md LIKE '<!-- starter:%'
           ORDER BY p.slug, (COALESCE(p.locale, '') = $2) DESC"#,
        ctx.wiki.id,
        ctx.content_locale
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| starter_label(&row.head).map(|label| (row.slug, label)))
        .collect())
}

/// The label of `<!-- starter: Label -->` on the first line.
pub(crate) fn starter_label(source: &str) -> Option<String> {
    let first = source.lines().next()?.trim();
    let label = first
        .strip_prefix("<!-- starter:")?
        .strip_suffix("-->")?
        .trim();
    (!label.is_empty() && label.chars().count() <= 80).then(|| label.to_string())
}

/// A starter's text as a new page receives it: without the starter line and
/// the template's own documentation.
pub(crate) fn starter_body(source: &str) -> String {
    let mut rest = match source.split_once('\n') {
        Some((first, rest)) if starter_label(first).is_some() => rest,
        _ => source,
    };
    if let Some((line, after)) = rest.split_once('\n')
        && line.trim_start().starts_with("<!-- title:")
    {
        rest = after;
    }
    let lower = rest.to_ascii_lowercase();
    let mut out = String::with_capacity(rest.len());
    let mut i = 0;
    while let Some(at) = lower[i..].find("<noinclude>") {
        let start = i + at;
        out.push_str(&rest[i..start]);
        i = match lower[start..].find("</noinclude>") {
            Some(end) => start + end + "</noinclude>".len(),
            None => rest.len(),
        };
    }
    out.push_str(&rest[i..]);
    out.trim_start_matches(['\n', '\r']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_starter_is_named_by_its_first_line() {
        assert_eq!(
            starter_label("<!-- starter: Статья о VTuber -->\nbody").as_deref(),
            Some("Статья о VTuber")
        );
        assert_eq!(starter_label("body\n<!-- starter: late -->"), None);
        assert_eq!(starter_label("<!-- starter:  -->"), None);
    }

    #[test]
    fn a_starter_body_drops_its_label_and_documentation() {
        let source = "<!-- starter: VTuber -->\n<!-- title: VTuber -->\n{{Infobox VTuber|name=}}\n<noinclude>How to use</noinclude>\n## Lore\n";
        assert_eq!(
            starter_body(source),
            "{{Infobox VTuber|name=}}\n\n## Lore\n"
        );
    }
}
