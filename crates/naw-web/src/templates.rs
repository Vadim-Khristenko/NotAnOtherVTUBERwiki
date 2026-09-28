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
    expand_in(&state.db, &Wiki::of(ctx), &notes(ctx), path, body).await
}

/// The failure notes in the reader's language, and the page language.
pub(crate) fn notes(ctx: &Ctx) -> transclude::Notes {
    transclude::Notes {
        missing: ctx.t_with("template.missing", &[("name", "{name}")]),
        looped: ctx.t_with("template.looped", &[("name", "{name}")]),
        limit: ctx.t("template.limit"),
        language: ctx.content_locale.clone(),
        bad_data: ctx.t_with(
            "template.bad_data",
            &[("line", "{line}"), ("reason", "{reason}")],
        ),
        unknown_fields: ctx.t_with("template.unknown_fields", &[("names", "{names}")]),
        missing_fields: ctx.t_with("template.missing_fields", &[("names", "{names}")]),
        yaml_reasons: naw_markdown::yaml::CODES
            .iter()
            .map(|code| (code.to_string(), ctx.t(&format!("template.yaml_{code}"))))
            .collect(),
    }
}

/// Where templates are looked up: the wiki, and the languages to prefer.
pub(crate) struct Wiki<'a> {
    pub id: Uuid,
    pub locale: &'a str,
    pub default_locale: &'a str,
}

impl<'a> Wiki<'a> {
    pub(crate) fn of(ctx: &'a Ctx) -> Self {
        Self {
            id: ctx.wiki.id,
            locale: &ctx.content_locale,
            default_locale: &ctx.wiki.default_locale,
        }
    }
}

/// [`expand`] without a request, for indexing from the command line.
pub(crate) async fn expand_in(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    notes: &transclude::Notes,
    path: &str,
    body: &str,
) -> Result<Expanded, AppError> {
    expand_with(db, wiki, notes, path, body, HashMap::new()).await
}

/// [`expand_in`] with some templates given rather than loaded: an edit of a
/// template previewed on a page that uses it.
pub(crate) async fn expand_with(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    notes: &transclude::Notes,
    path: &str,
    body: &str,
    given: HashMap<String, String>,
) -> Result<Expanded, AppError> {
    let source = if crate::pages::split_path(path).0 == "template" {
        transclude::template_view(body)
    } else {
        body.to_string()
    };
    if !source.contains("{{") {
        return finish(db, wiki, source, Vec::new()).await;
    }
    let run = run_rounds(db, wiki, notes, &source, given).await?;
    finish(db, wiki, run.text, run.used.into_iter().collect()).await
}

/// Expands `source`, loading in rounds what the templates of the last round
/// call, with `given` in place of what is stored.
async fn run_rounds(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    notes: &transclude::Notes,
    source: &str,
    given: HashMap<String, String>,
) -> Result<transclude::Expansion, AppError> {
    let mut tried: HashSet<String> = given.keys().cloned().collect();
    let mut loaded = given;
    let mut labels = transclude::Labels::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut names_tried: HashSet<String> = HashSet::new();
    if !tried.is_empty() {
        let wanted: Vec<String> = tried.iter().cloned().collect();
        labels.extend(load(db, wiki, &wanted).await?.labels);
    }
    for _ in 0..transclude::DEPTH_MAX {
        let run = transclude::expand_full(source, &loaded, &labels, &aliases, notes);
        // names in another script (`{{Карточка VTuber}}`) are looked up by title
        let names: Vec<String> = run
            .unresolved
            .iter()
            .filter(|name| !names_tried.contains(*name))
            .take(TEMPLATES_MAX)
            .cloned()
            .collect();
        names_tried.extend(names.iter().cloned());
        let found_names = if names.is_empty() {
            HashMap::new()
        } else {
            titles(db, wiki, &names).await?
        };
        let room = TEMPLATES_MAX.saturating_sub(tried.len());
        let wanted: Vec<String> = run
            .missing
            .iter()
            .chain(found_names.values())
            .filter(|slug| !tried.contains(*slug))
            .take(room)
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        aliases.extend(found_names);
        if wanted.is_empty() && names.is_empty() {
            return Ok(run);
        }
        tried.extend(wanted.iter().cloned());
        if !wanted.is_empty() {
            let found = load(db, wiki, &wanted).await?;
            loaded.extend(found.code);
            labels.extend(found.labels);
        }
    }
    Ok(transclude::expand_full(
        source, &loaded, &labels, &aliases, notes,
    ))
}

/// Templates of this wiki whose title in some language is one of `names`
/// (as [`transclude::alias_key`]s), by name; the oldest wins a tie.
async fn titles(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    names: &[String],
) -> Result<HashMap<String, String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.deleted_at IS NULL
             AND lower(regexp_replace(btrim(replace(p.title, '_', ' ')), '\s+', ' ', 'g')) = ANY($2)
           ORDER BY p.created_at"#,
        wiki.id,
        names
    )
    .fetch_all(db)
    .await?;
    let mut out = HashMap::new();
    for row in rows {
        if let Some(key) = transclude::alias_key(&row.title)
            && names.contains(&key)
        {
            out.entry(key).or_insert(row.slug);
        }
    }
    Ok(out)
}

/// Why a template version cannot be saved as it is, as a message key, or
/// `None`. A translation carries no code: every language runs the main
/// version's, so translating a template cannot make it a different template.
/// The main version must not end up calling itself, or every page using it
/// would show the loop instead.
pub(crate) async fn refusal(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    notes: &transclude::Notes,
    slug: &str,
    locale: &str,
    body: &str,
) -> Result<Option<&'static str>, AppError> {
    // A starter lays out a whole new page: its body is an article, and each
    // language writes its own. The rules below are for components.
    if body
        .lines()
        .next()
        .is_some_and(|first| starter_label(first).is_some())
    {
        return Ok(None);
    }
    let main = main_locale(db, wiki, slug).await?;
    let is_main = match &main {
        None => true,
        Some(main) => main == locale || locale == wiki.default_locale,
    };
    if !is_main {
        return Ok(transclude::has_code(body).then_some("template.translation_code"));
    }
    let given = HashMap::from([(slug.to_string(), body.to_string())]);
    let run = run_rounds(db, wiki, notes, &format!("{{{{{slug}}}}}"), given).await?;
    Ok(run.looped.contains(slug).then_some("template.loops_itself"))
}

/// A template's main version: its language, title and source.
pub(crate) struct Main {
    pub locale: String,
    pub title: String,
    pub body: String,
}

/// The main version of `slug` (see [`main_locale`]), or `None`.
pub(crate) async fn main_version(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    slug: &str,
) -> Result<Option<Main>, AppError> {
    let row = sqlx::query!(
        r#"SELECT COALESCE(p.locale, '') AS "locale!", p.title, r.body_md
           FROM pages p JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.slug = $2
             AND p.deleted_at IS NULL
           ORDER BY (COALESCE(p.locale, '') = $3) DESC, p.created_at
           LIMIT 1"#,
        wiki.id,
        slug,
        wiki.default_locale
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Main {
        locale: r.locale,
        title: r.title,
        body: r.body_md,
    }))
}

/// The default of each `{{#label:key|Default}}` in a template's code.
pub(crate) fn label_defaults(code: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut rest = code;
    while let Some(at) = rest.find("{{#label:") {
        rest = &rest[at + "{{#label:".len()..];
        let Some(end) = rest.find("}}") else { break };
        let inside = &rest[..end];
        if let Some((key, default)) = inside.split_once('|') {
            out.entry(key.trim().to_string())
                .or_insert_with(|| default.trim().to_string());
        }
        rest = &rest[end..];
    }
    out
}

/// The language of a template's main version: the wiki's own when it has
/// one, else the first written. `None` for a template nobody wrote yet.
pub(crate) async fn main_locale(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    slug: &str,
) -> Result<Option<String>, AppError> {
    let locale = sqlx::query_scalar!(
        r#"SELECT COALESCE(p.locale, '') AS "locale!" FROM pages p
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.slug = $2
             AND p.deleted_at IS NULL
           ORDER BY (COALESCE(p.locale, '') = $3) DESC, p.created_at
           LIMIT 1"#,
        wiki.id,
        slug,
        wiki.default_locale
    )
    .fetch_optional(db)
    .await?;
    Ok(locale)
}

/// The last step for any text: `image:name` and its siblings point at files.
async fn finish(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    text: String,
    used: Vec<String>,
) -> Result<Expanded, AppError> {
    let text = crate::files::resolve(db, wiki.id, text).await?;
    Ok(Expanded { text, used })
}

/// What a round of loading brings: each template's code, from its main
/// version, and its labels in the reader's language.
struct Loaded {
    code: HashMap<String, String>,
    labels: transclude::Labels,
}

/// The live versions of each template. The code always comes from the main
/// version (see [`main_locale`]), so every language runs the same template;
/// the version in the reader's language adds only its labels.
async fn load(db: &sqlx::PgPool, wiki: &Wiki<'_>, slugs: &[String]) -> Result<Loaded, AppError> {
    let rows = sqlx::query!(
        r#"SELECT p.slug AS "slug!", COALESCE(p.locale, '') AS "locale!", r.body_md AS "body!"
           FROM pages p JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.slug = ANY($2)
             AND p.deleted_at IS NULL
           ORDER BY p.slug, (COALESCE(p.locale, '') = $3) DESC, p.created_at"#,
        wiki.id,
        slugs,
        wiki.default_locale
    )
    .fetch_all(db)
    .await?;
    let mut code = HashMap::new();
    let mut labels = transclude::Labels::new();
    for row in rows {
        // the first row of each slug is its main version
        if !code.contains_key(&row.slug) {
            code.insert(row.slug.clone(), row.body.clone());
        }
        if row.locale == wiki.locale {
            let found = transclude::labels_of(&row.body);
            if !found.is_empty() {
                labels.insert(row.slug, found);
            }
        }
    }
    Ok(Loaded { code, labels })
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
    /// The page path for an article or a template; empty for a profile.
    pub path: String,
}

/// How many pages use `slug`, and the first of them by title.
pub(crate) async fn uses(
    state: &AppState,
    ctx: &Ctx,
    slug: &str,
) -> Result<(i64, Vec<Use>), AppError> {
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM template_uses u JOIN pages p ON p.id = u.page_id
           WHERE u.wiki_id = $1 AND u.template_slug = $2 AND p.deleted_at IS NULL
             AND NOT (p.namespace = 'template' AND p.slug = $2)"#,
        ctx.wiki.id,
        slug
    )
    .fetch_one(&state.db)
    .await?;
    let rows = sqlx::query!(
        r#"SELECT p.title, p.slug, p.namespace::text AS "namespace!"
           FROM template_uses u JOIN pages p ON p.id = u.page_id
           WHERE u.wiki_id = $1 AND u.template_slug = $2 AND p.deleted_at IS NULL
             AND NOT (p.namespace = 'template' AND p.slug = $2)
           ORDER BY p.title LIMIT $3"#,
        ctx.wiki.id,
        slug,
        USES_SHOWN
    )
    .fetch_all(&state.db)
    .await?;
    let shown = rows
        .into_iter()
        .map(|row| {
            let path = match row.namespace.as_str() {
                "template" => format!("{}{}", crate::pages::TEMPLATE_PREFIX, row.slug),
                "main" => row.slug.clone(),
                _ => String::new(),
            };
            Use {
                href: if path.is_empty() {
                    format!("/user/{}", row.slug)
                } else {
                    ctx.link(&format!("/{path}"))
                },
                title: row.title,
                path,
            }
        })
        .collect();
    Ok((total, shown))
}

/// A page template (a starter) as New page offers it.
pub(crate) struct Starter {
    pub slug: String,
    pub label: String,
    /// The first paragraph of its `<noinclude>` note, as plain text.
    pub note: String,
}

/// Templates a new page can start from: those whose source begins with
/// `<!-- starter: Label -->`. The comment never renders. Each comes in the
/// reader's language when it has one, else the wiki's, else the first written.
pub(crate) async fn starters(state: &AppState, ctx: &Ctx) -> Result<Vec<Starter>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (p.slug) p.slug AS "slug!", r.body_md AS "body!"
           FROM pages p JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.deleted_at IS NULL
             AND r.body_md LIKE '<!-- starter:%'
           ORDER BY p.slug, (COALESCE(p.locale, '') = $2) DESC,
                    (COALESCE(p.locale, '') = $3) DESC, p.created_at"#,
        ctx.wiki.id,
        ctx.content_locale,
        ctx.wiki.default_locale
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            starter_label(&row.body).map(|label| Starter {
                note: starter_note(&row.body),
                slug: row.slug,
                label,
            })
        })
        .collect())
}

/// A starter's source for a new page, with the same language preference as
/// [`starters`], so a starter nobody translated still fills the page.
pub(crate) async fn starter_source(
    db: &sqlx::PgPool,
    wiki: &Wiki<'_>,
    slug: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT r.body_md FROM pages p JOIN revisions r ON r.id = p.current_revision_id
           WHERE p.wiki_id = $1 AND p.namespace = 'template' AND p.slug = $2
             AND p.deleted_at IS NULL AND r.body_md LIKE '<!-- starter:%'
           ORDER BY (COALESCE(p.locale, '') = $3) DESC, (COALESCE(p.locale, '') = $4) DESC,
                    p.created_at
           LIMIT 1"#,
        wiki.id,
        slug,
        wiki.locale,
        wiki.default_locale
    )
    .fetch_optional(db)
    .await?)
}

/// The first paragraph of a starter's `<noinclude>` note, without Markdown
/// emphasis, for the list on New page.
pub(crate) fn starter_note(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    let Some(start) = lower.find("<noinclude>") else {
        return String::new();
    };
    let start = start + "<noinclude>".len();
    let end = lower[start..]
        .find("</noinclude>")
        .map_or(source.len(), |e| start + e);
    let paragraph = source[start..end]
        .trim()
        .split("\n\n")
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let plain: String = paragraph
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '`'))
        .collect();
    plain.chars().take(240).collect()
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

/// With a database: a template belongs to one wiki, runs one code in every
/// language, and a save that would break that is refused.
#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::PgPool;

    async fn wiki(db: &PgPool, slug: &str, default_locale: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO wikis (id, slug, name, default_locale) VALUES ($1, $2, $2, $3)")
            .bind(id)
            .bind(slug)
            .bind(default_locale)
            .execute(db)
            .await
            .expect("wiki");
        id
    }

    /// A live template version; `age` seconds old, so the first written is known.
    async fn template(db: &PgPool, wiki_id: Uuid, slug: &str, locale: &str, body: &str, age: i64) {
        let page = Uuid::new_v4();
        let revision = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale, created_at)
             VALUES ($1, $2, 'template', $3, $3, $4, now() - make_interval(secs => $5))",
        )
        .bind(page)
        .bind(wiki_id)
        .bind(slug)
        .bind(locale)
        .bind(age as f64)
        .execute(db)
        .await
        .expect("page");
        sqlx::query(
            "INSERT INTO revisions (id, page_id, body_md, content_hash) VALUES ($1, $2, $3, $4)",
        )
        .bind(revision)
        .bind(page)
        .bind(body)
        .bind(vec![0u8])
        .execute(db)
        .await
        .expect("revision");
        sqlx::query("UPDATE pages SET current_revision_id = $2 WHERE id = $1")
            .bind(page)
            .bind(revision)
            .execute(db)
            .await
            .expect("current");
    }

    async fn render(
        db: &PgPool,
        wiki_id: Uuid,
        locale: &str,
        default_locale: &str,
        body: &str,
    ) -> String {
        let wiki = Wiki {
            id: wiki_id,
            locale,
            default_locale,
        };
        expand_in(db, &wiki, &transclude::Notes::default(), "article", body)
            .await
            .expect("expand")
            .text
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_template_never_leaves_its_wiki(db: PgPool) {
        let a = wiki(&db, "a", "en").await;
        let b = wiki(&db, "b", "en").await;
        let c = wiki(&db, "c", "en").await;
        template(&db, a, "card", "en", "from A", 10).await;
        template(&db, b, "card", "en", "from B", 10).await;
        assert_eq!(render(&db, a, "en", "en", "{{card}}").await, "from A");
        assert_eq!(render(&db, b, "en", "en", "{{card}}").await, "from B");
        // a wiki without it gets the missing note, not a neighbour's template
        let missing = render(&db, c, "en", "en", "{{card}}").await;
        assert!(
            !missing.contains("from A") && !missing.contains("from B"),
            "{missing}"
        );
        assert!(
            main_version(
                &db,
                &Wiki {
                    id: c,
                    locale: "en",
                    default_locale: "en"
                },
                "card"
            )
            .await
            .expect("q")
            .is_none()
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn every_language_runs_the_main_code_with_its_own_labels(db: PgPool) {
        let w = wiki(&db, "w", "en").await;
        template(
            &db,
            w,
            "card",
            "en",
            "<includeonly>{{#label:debut|Debut}}: {{{debut|}}}</includeonly>",
            20,
        )
        .await;
        // a translation with words only
        template(
            &db,
            w,
            "card",
            "ru",
            "<noinclude>Описание</noinclude>\n<labels>\ndebut = Дебют\n</labels>",
            10,
        )
        .await;
        let call = "{{card|debut=2021}}";
        assert_eq!(render(&db, w, "en", "en", call).await, "Debut: 2021");
        assert_eq!(render(&db, w, "ru", "en", call).await, "Дебют: 2021");
        // a language with no translation still runs the same template
        assert_eq!(render(&db, w, "de", "en", call).await, "Debut: 2021");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_translation_that_carries_code_is_ignored_and_refused(db: PgPool) {
        let w = wiki(&db, "w", "en").await;
        template(&db, w, "card", "en", "<includeonly>real</includeonly>", 20).await;
        // written before the rule: its code must not run for Russian readers
        template(&db, w, "card", "ru", "other code", 10).await;
        assert_eq!(render(&db, w, "ru", "en", "{{card}}").await, "real");
        let wiki_ru = Wiki {
            id: w,
            locale: "ru",
            default_locale: "en",
        };
        let notes = transclude::Notes::default();
        let refused = refusal(
            &db,
            &wiki_ru,
            &notes,
            "card",
            "ru",
            "<includeonly>mine</includeonly>",
        )
        .await
        .expect("q");
        assert_eq!(refused, Some("template.translation_code"));
        let words = refusal(
            &db,
            &wiki_ru,
            &notes,
            "card",
            "ru",
            "<noinclude>Описание</noinclude>\n<labels>\na = б\n</labels>",
        )
        .await
        .expect("q");
        assert_eq!(words, None);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn the_main_version_cannot_call_itself(db: PgPool) {
        let w = wiki(&db, "w", "en").await;
        template(&db, w, "card", "en", "<includeonly>fine</includeonly>", 20).await;
        template(
            &db,
            w,
            "wrapper",
            "en",
            "<includeonly>{{card}}</includeonly>",
            20,
        )
        .await;
        let wiki_en = Wiki {
            id: w,
            locale: "en",
            default_locale: "en",
        };
        let notes = transclude::Notes::default();
        // directly
        let direct = refusal(&db, &wiki_en, &notes, "card", "en", "x {{card}}")
            .await
            .expect("q");
        assert_eq!(direct, Some("template.loops_itself"));
        // through another template
        let around = refusal(&db, &wiki_en, &notes, "card", "en", "x {{wrapper}}")
            .await
            .expect("q");
        assert_eq!(around, Some("template.loops_itself"));
        // an example of itself in its own documentation is fine
        let docs = refusal(
            &db,
            &wiki_en,
            &notes,
            "card",
            "en",
            "<includeonly>x</includeonly><noinclude>{{card}}</noinclude>",
        )
        .await
        .expect("q");
        assert_eq!(docs, None);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_starter_is_an_article_and_translates_freely(db: PgPool) {
        let w = wiki(&db, "w", "en").await;
        template(
            &db,
            w,
            "vtuber-article",
            "en",
            "<!-- starter: VTuber article -->\n# Name",
            20,
        )
        .await;
        let wiki_ru = Wiki {
            id: w,
            locale: "ru",
            default_locale: "en",
        };
        let ru = "<!-- starter: Статья о VTuber -->\n# Имя\n\n{{Infobox VTuber}}";
        let result = refusal(
            &db,
            &wiki_ru,
            &transclude::Notes::default(),
            "vtuber-article",
            "ru",
            ru,
        )
        .await
        .expect("q");
        assert_eq!(result, None);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn the_first_version_is_main_until_the_wiki_language_has_one(db: PgPool) {
        let w = wiki(&db, "w", "en").await;
        let wiki_en = Wiki {
            id: w,
            locale: "en",
            default_locale: "en",
        };
        template(
            &db,
            w,
            "card",
            "ru",
            "<includeonly>ru code</includeonly>",
            20,
        )
        .await;
        assert_eq!(
            main_locale(&db, &wiki_en, "card")
                .await
                .expect("q")
                .as_deref(),
            Some("ru")
        );
        assert_eq!(render(&db, w, "en", "en", "{{card}}").await, "ru code");
        template(
            &db,
            w,
            "card",
            "en",
            "<includeonly>en code</includeonly>",
            10,
        )
        .await;
        assert_eq!(
            main_locale(&db, &wiki_en, "card")
                .await
                .expect("q")
                .as_deref(),
            Some("en")
        );
        assert_eq!(render(&db, w, "ru", "en", "{{card}}").await, "en code");
    }

    async fn retitle(db: &PgPool, wiki_id: Uuid, slug: &str, locale: &str, title: &str) {
        sqlx::query("UPDATE pages SET title = $4 WHERE wiki_id = $1 AND slug = $2 AND locale = $3")
            .bind(wiki_id)
            .bind(slug)
            .bind(locale)
            .bind(title)
            .execute(db)
            .await
            .expect("title");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_template_is_called_by_its_title_in_its_own_wiki(db: PgPool) {
        let w = wiki(&db, "w", "en").await;
        let other = wiki(&db, "other", "en").await;
        template(
            &db,
            w,
            "infobox-vtuber",
            "en",
            "<includeonly>card {{{name|}}}</includeonly>",
            20,
        )
        .await;
        template(
            &db,
            w,
            "infobox-vtuber",
            "ru",
            "<labels>\nname = Имя\n</labels>",
            10,
        )
        .await;
        retitle(&db, w, "infobox-vtuber", "ru", "Карточка VTuber").await;
        // the same title in another wiki must never answer for this one
        template(
            &db,
            other,
            "stranger",
            "en",
            "<includeonly>not ours</includeonly>",
            20,
        )
        .await;
        retitle(&db, other, "stranger", "en", "Чужая карточка").await;
        let call = "{{Карточка VTuber | name = Филиан}} {{Шаблон:Карточка_VTuber}}";
        assert_eq!(render(&db, w, "ru", "en", call).await, "card Филиан card ");
        let stranger = render(&db, w, "ru", "en", "{{Чужая карточка}}").await;
        assert!(!stranger.contains("not ours"), "{stranger}");
        // YAML with Russian field names through a Russian title
        template(
            &db,
            w,
            "karta",
            "en",
            "<includeonly>{{{имя|}}} / {{{дебют|}}}</includeonly>",
            20,
        )
        .await;
        retitle(&db, w, "karta", "en", "Карта").await;
        let yaml = "{{Карта\n```yaml\nимя: Филиан\nдебют: 2021\n```\n}}";
        assert_eq!(render(&db, w, "ru", "en", yaml).await, "Филиан / 2021");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_page_template_fills_a_new_page_in_any_language(db: PgPool) {
        let w = wiki(&db, "w", "en").await;
        let other = wiki(&db, "other", "en").await;
        template(
            &db,
            w,
            "vtuber-article",
            "en",
            "<!-- starter: VTuber article -->\n# Name",
            20,
        )
        .await;
        template(
            &db,
            other,
            "vtuber-article",
            "en",
            "<!-- starter: Elsewhere -->\n# Other wiki",
            20,
        )
        .await;
        let in_ru = Wiki {
            id: w,
            locale: "ru",
            default_locale: "en",
        };
        // no Russian version yet: the page still starts from the English one
        let body = starter_source(&db, &in_ru, "vtuber-article")
            .await
            .expect("q")
            .expect("found");
        assert!(body.contains("# Name"), "{body}");
        template(
            &db,
            w,
            "vtuber-article",
            "ru",
            "<!-- starter: Статья о VTuber -->\n# Имя",
            10,
        )
        .await;
        let body = starter_source(&db, &in_ru, "vtuber-article")
            .await
            .expect("q")
            .expect("found");
        assert!(body.contains("# Имя"), "{body}");
        // a component is not a page template
        template(&db, w, "card", "en", "<includeonly>x</includeonly>", 10).await;
        assert!(
            starter_source(&db, &in_ru, "card")
                .await
                .expect("q")
                .is_none()
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn yaml_fields_follow_the_schema_of_the_main_version(db: PgPool) {
        let w = wiki(&db, "w", "en").await;
        let code = "<params>\nname: text required\nfans: list\n</params>\n<includeonly>{{#label:fans|Fans}}: {{{fans|}}} ({{{name|}}})</includeonly>";
        template(&db, w, "card", "en", code, 20).await;
        template(
            &db,
            w,
            "card",
            "ru",
            "<labels>\nfans = Фанаты\n</labels>",
            10,
        )
        .await;
        let yaml = "{{Card\n```yaml\nname: Filian\nfans:\n  - Snackers\n  - Cats\n```\n}}";
        assert_eq!(
            render(&db, w, "ru", "en", yaml).await,
            "Фанаты: Snackers, Cats (Filian)"
        );
        let off = render(
            &db,
            w,
            "en",
            "en",
            "{{Card\n```yaml\nfans: x\ncolour: red\n```\n}}",
        )
        .await;
        assert!(off.contains("does not have: colour"), "{off}");
        assert!(off.contains("left empty: name"), "{off}");
    }
}
