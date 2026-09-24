//! What the About page adds under its own text, from the database, so it is
//! never out of date: the wiki in numbers, who looks after it, its real
//! addresses and the engine it runs on.

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::pages::ENGINE_VERSION;
use crate::resolve::Ctx;

/// The engine's public source.
const SOURCE_URL: &str = "https://github.com/Vadim-Khristenko/NotAnOtherVTUBERwiki";

/// The About page's slug, from the wiki's settings.
pub(crate) fn slug(ctx: &Ctx) -> String {
    ctx.wiki
        .settings
        .get("about_slug")
        .and_then(|v| v.as_str())
        .filter(|s| crate::pages::slug_is_valid(s))
        .unwrap_or("about")
        .to_string()
}

/// The facts block for the About page.
pub(crate) async fn facts(state: &AppState, ctx: &Ctx) -> Result<minijinja::Value, AppError> {
    let counts = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL) AS "articles!",
             (SELECT count(*) FROM pages WHERE wiki_id = $1 AND namespace = 'template' AND deleted_at IS NULL) AS "templates!",
             (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id WHERE p.wiki_id = $1) AS "edits!",
             (SELECT count(DISTINCT r.author_id) FROM revisions r JOIN pages p ON p.id = r.page_id
                WHERE p.wiki_id = $1 AND r.author_id IS NOT NULL) AS "authors!",
             (SELECT count(*) FROM media WHERE wiki_id = $1) AS "files!",
             (SELECT count(*) FROM emotes WHERE wiki_id = $1) AS "emotes!",
             (SELECT count(DISTINCT COALESCE(locale, '')) FROM pages
                WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL) AS "languages!",
             (SELECT min(r.created_at) FROM revisions r JOIN pages p ON p.id = r.page_id
                WHERE p.wiki_id = $1) AS since"#,
        ctx.wiki.id
    )
    .fetch_one(&state.db)
    .await?;

    // The people who look after the wiki, strongest role first.
    let team = sqlx::query!(
        r#"SELECT u.username, u.display_name, m.role::text AS "role!"
           FROM wiki_memberships m JOIN users u ON u.id = m.user_id
           WHERE m.wiki_id = $1 AND m.role IN ('owner', 'admin', 'moderator', 'curator')
           ORDER BY m.role DESC, lower(u.username)"#,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let mut groups: Vec<(String, Vec<minijinja::Value>)> = Vec::new();
    for person in team {
        let member = minijinja::context! {
            username => person.username.clone(),
            name => person.display_name.unwrap_or(person.username),
        };
        match groups.last_mut() {
            Some((role, people)) if *role == person.role => people.push(member),
            _ => groups.push((person.role, vec![member])),
        }
    }
    let team = groups
        .into_iter()
        .map(|(role, people)| {
            minijinja::context! {
                label => ctx.tn_with(&format!("about.role_{role}"), people.len() as i64, &[]),
                people => people,
            }
        })
        .collect::<Vec<_>>();

    Ok(minijinja::context! {
        articles => counts.articles,
        templates => counts.templates,
        edits => counts.edits,
        authors => counts.authors,
        files => counts.files,
        emotes => counts.emotes,
        languages => counts.languages,
        since => counts.since.map(|at| ctx.day(at)),
        team => team,
        domains => crate::chrome::domains(&ctx.wiki.settings),
        version => ENGINE_VERSION,
        source_url => SOURCE_URL,
    })
}
