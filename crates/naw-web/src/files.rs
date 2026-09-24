//! A page for every uploaded file, as on Wikimedia Commons: `/image:ferris.png`,
//! `/audio:theme.ogg`, `/video:clip.webm`, `/file:notes.pdf`.
//!
//! The prefix follows the file's type and any other prefix redirects to it.
//! The page shows the file (a picture, a player, or a download), what it is,
//! who uploaded it, and where it is used. Its description is an ordinary page
//! in the `file` namespace under the same name, with its own history.
//!
//! In an article, `![Ferris](image:ferris.png)` shows the file and
//! `[notes](file:notes.pdf)` links to its page; pictures link to their page too.

use std::collections::HashMap;

use axum::http::HeaderMap;
use axum::response::Response;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::pages::{self, ENGINE_VERSION};
use crate::resolve::Ctx;

/// Path prefixes, each with the kind of file it shows.
const PREFIXES: [(&str, &str); 4] = [
    ("image", "image"),
    ("audio", "audio"),
    ("video", "video"),
    ("file", "document"),
];

/// The prefix a file's page lives under.
pub(crate) fn prefix_of(class: &str) -> &'static str {
    match class {
        "image" => "image",
        "audio" => "audio",
        "video" => "video",
        _ => "file",
    }
}

/// `image:ferris.png` as its prefix and name, when it names a file page.
pub(crate) fn split(path: &str) -> Option<(&'static str, &str)> {
    let (prefix, name) = path.split_once(':')?;
    let prefix = PREFIXES.iter().find(|(p, _)| *p == prefix)?.0;
    name_is_valid(name).then_some((prefix, name))
}

/// A file page name: lowercase letters, digits and dashes, a dot, and an
/// extension of letters and digits.
pub(crate) fn name_is_valid(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    name.len() <= 100
        && !stem.is_empty()
        && !ext.is_empty()
        && ext.len() <= 5
        && stem
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && ext
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The name an uploaded file's page starts from: its file name without the
/// extension, Cyrillic spelled in Latin letters, everything else a dash.
pub(crate) fn stem_of(filename: &str) -> String {
    let base = filename
        .rsplit_once('.')
        .map_or(filename, |(stem, _)| stem)
        .to_lowercase();
    let mut out = String::with_capacity(base.len());
    for c in base.chars() {
        let piece: &str = match c {
            'a'..='z' | '0'..='9' => {
                out.push(c);
                continue;
            }
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'ё' | 'э' => "e",
            'ж' => "zh",
            'з' => "z",
            'и' | 'й' => "i",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "h",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "sch",
            'ы' => "y",
            'ю' => "yu",
            'я' => "ya",
            'ъ' | 'ь' => "",
            _ => "-",
        };
        out.push_str(piece);
    }
    let mut stem = String::with_capacity(out.len());
    for c in out.chars() {
        if c == '-' && (stem.is_empty() || stem.ends_with('-')) {
            continue;
        }
        stem.push(c);
    }
    let stem: String = stem.trim_end_matches('-').chars().take(80).collect();
    let stem = stem.trim_end_matches('-');
    if stem.is_empty() {
        "file".to_string()
    } else {
        stem.to_string()
    }
}

/// `stem.ext` for the first try, `stem-2.ext` and on after it.
pub(crate) fn numbered(stem: &str, n: u32, ext: &str) -> String {
    if n <= 1 {
        format!("{stem}.{ext}")
    } else {
        format!("{stem}-{n}.{ext}")
    }
}

/// One file of this wiki.
struct File {
    storage_key: String,
    name: String,
    kind: String,
}

async fn files_named(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    names: &[String],
) -> Result<HashMap<String, File>, AppError> {
    let rows = sqlx::query!(
        "SELECT storage_key, name, kind FROM media WHERE wiki_id = $1 AND name = ANY($2)",
        wiki_id,
        names
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.name.clone(),
                File {
                    storage_key: row.storage_key,
                    name: row.name,
                    kind: row.kind,
                },
            )
        })
        .collect())
}

/// Points `image:name` style destinations at the wiki's files: an image
/// shows the file, a link goes to its page. A name with no file becomes a
/// link to the page, which says so.
pub(crate) async fn resolve(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    markdown: String,
) -> Result<String, AppError> {
    const WANTED: [&str; 4] = ["image:", "audio:", "video:", "file:"];
    if !WANTED.iter().any(|p| markdown.contains(p)) {
        return Ok(markdown);
    }
    let found = naw_markdown::prefixed_destinations(&markdown, &WANTED);
    if found.is_empty() {
        return Ok(markdown);
    }
    let mut names: Vec<String> = found
        .iter()
        .filter_map(|(_, dest, _)| split(&dest.to_ascii_lowercase()).map(|(_, n)| n.to_string()))
        .collect();
    names.sort();
    names.dedup();
    let files = files_named(db, wiki_id, &names).await?;
    let mut out = markdown;
    // From the end, so earlier ranges stay valid.
    for (range, dest, is_image) in found.into_iter().rev() {
        let lower = dest.to_ascii_lowercase();
        let Some((_, name)) = split(&lower) else {
            continue;
        };
        let target = match files.get(name) {
            Some(file) if is_image => crate::media::url_for_key(&file.storage_key),
            Some(file) => format!("/{}:{}", prefix_of(&file.kind), file.name),
            None => format!("/{lower}"),
        };
        out.replace_range(range, &target);
    }
    Ok(out)
}

/// Storage keys of this wiki's files that rendered HTML shows or links to.
pub(crate) fn keys_in_html(html: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("\"/media/") {
        let tail = &rest[at + 1..];
        let end = tail.find('"').unwrap_or(tail.len());
        let path = &tail[..end];
        // Only uploads, which live under a two character directory.
        if let Some(inner) = path.strip_prefix("/media/")
            && inner.len() > 3
            && inner.as_bytes()[2] == b'/'
        {
            keys.push(format!("media/{inner}"));
        }
        rest = &tail[end.min(tail.len())..];
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Wraps each picture from the wiki's uploads in a link to its file page,
/// the way an encyclopedia does, unless it is a link already.
pub(crate) async fn link_images(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    html: String,
) -> Result<String, AppError> {
    if !html.contains("<img") {
        return Ok(html);
    }
    let keys = keys_in_html(&html);
    if keys.is_empty() {
        return Ok(html);
    }
    let rows = sqlx::query!(
        "SELECT storage_key, name, kind FROM media WHERE wiki_id = $1 AND storage_key = ANY($2)",
        wiki_id,
        &keys
    )
    .fetch_all(db)
    .await?;
    let pages: HashMap<String, String> = rows
        .into_iter()
        .map(|row| {
            (
                crate::media::url_for_key(&row.storage_key),
                format!("/{}:{}", prefix_of(&row.kind), row.name),
            )
        })
        .collect();
    let mut out = String::with_capacity(html.len() + keys.len() * 48);
    let mut rest = html.as_str();
    // Whether the text so far left a link open, carried from chunk to chunk.
    let mut in_link = false;
    while let Some(at) = rest.find("<img ") {
        let before = &rest[..at];
        let Some(close) = rest[at..].find('>') else {
            break;
        };
        let tag = &rest[at..at + close + 1];
        let src = tag
            .split(" src=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        match (before.rfind("<a "), before.rfind("</a>")) {
            (Some(open), close) => in_link = close.is_none_or(|close| close < open),
            (None, Some(_)) => in_link = false,
            (None, None) => {}
        }
        out.push_str(before);
        match pages.get(src) {
            Some(page) if !in_link => {
                out.push_str("<a class=\"file-link\" href=\"");
                out.push_str(&naw_core::html::escape(page));
                out.push_str("\">");
                out.push_str(tag);
                out.push_str("</a>");
            }
            _ => out.push_str(tag),
        }
        rest = &rest[at + close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Replaces the list of files `page_id` shows, inside the caller's transaction.
pub(crate) async fn record_uses(
    conn: &mut sqlx::PgConnection,
    wiki_id: Uuid,
    page_id: Uuid,
    html: &str,
) -> Result<(), AppError> {
    let keys = keys_in_html(html);
    sqlx::query!("DELETE FROM file_uses WHERE page_id = $1", page_id)
        .execute(&mut *conn)
        .await?;
    if !keys.is_empty() {
        sqlx::query!(
            "INSERT INTO file_uses (page_id, wiki_id, storage_key)
             SELECT $1, $2, unnest($3::text[]) ON CONFLICT DO NOTHING",
            page_id,
            wiki_id,
            &keys
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Bytes as a reader counts them.
fn human_size(ctx: &Ctx, bytes: i64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 {
        ctx.t_with(
            "file.size_mb",
            &[("n", &format!("{:.1}", b / 1024.0 / 1024.0))],
        )
    } else if b >= 1024.0 {
        ctx.t_with("file.size_kb", &[("n", &format!("{:.0}", b / 1024.0))])
    } else {
        ctx.t_with("file.size_b", &[("n", &bytes.to_string())])
    }
}

/// Pages that use a file, at most.
const USES_SHOWN: i64 = 100;

/// GET /image:name and its siblings.
pub(crate) async fn page(
    state: &AppState,
    ctx: &Ctx,
    headers: &HeaderMap,
    prefix: &str,
    name: &str,
) -> Result<Response, AppError> {
    let Some(file) = sqlx::query!(
        r#"SELECT m.storage_key, m.name, m.kind, m.mime, m.filename, m.size_bytes, m.width,
                  m.height, m.created_at,
                  (SELECT u.username FROM users u WHERE u.id = m.uploader_id) AS "uploader?"
           FROM media m
           WHERE m.wiki_id = $1 AND m.name = $2"#,
        ctx.wiki.id,
        name
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(crate::errors::not_found());
    };
    let canonical = prefix_of(&file.kind);
    if canonical != prefix {
        return Ok(pages::see_other(&ctx.link(&format!("/{canonical}:{name}"))));
    }
    let path = format!("{canonical}:{name}");
    let description = pages::find_page(&state.db, ctx.wiki.id, &path, &ctx.content_locale).await?;
    let description_html = match &description {
        Some(page) => Some(
            pages::cached_body(state, ctx, &path, &page.body_md)
                .await?
                .0,
        ),
        None => None,
    };
    let uses = sqlx::query!(
        r#"SELECT p.title, p.slug, p.namespace::text AS "namespace!"
           FROM file_uses f JOIN pages p ON p.id = f.page_id
           WHERE f.wiki_id = $1 AND f.storage_key = $2 AND p.deleted_at IS NULL
           ORDER BY p.title LIMIT $3"#,
        ctx.wiki.id,
        file.storage_key,
        USES_SHOWN
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|row| {
        let href = match row.namespace.as_str() {
            "user" => format!("/user/{}", row.slug),
            "template" => ctx.link(&format!("/template:{}", row.slug)),
            "file" => ctx.link(&format!("/file:{}", row.slug)),
            _ => ctx.link(&format!("/{}", row.slug)),
        };
        minijinja::context! { title => row.title, href => href }
    })
    .collect::<Vec<_>>();

    let url = crate::media::url_for_key(&file.storage_key);
    let alt: String = name
        .rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .replace('-', " ");
    let hash = file
        .storage_key
        .rsplit('/')
        .next()
        .and_then(|f| f.split('.').next())
        .unwrap_or("")
        .to_string();
    let may_edit = ctx
        .actor
        .can_edit_page(description.as_ref().and_then(|d| d.protection));
    let template = ctx
        .skin
        .env
        .get_template("file.html")
        .map_err(pages::template_error)?;
    let heading = ctx.t_with(&format!("file.prefix_{canonical}"), &[("name", name)]);
    let html = template
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => heading.clone(),
                version => ENGINE_VERSION,
                heading => heading,
                slug => path.clone(),
                kind => file.kind,
                url => url,
                mime => file.mime,
                original_name => file.filename,
                size => human_size(ctx, file.size_bytes),
                width => file.width,
                height => file.height,
                uploader => file.uploader,
                uploaded_at => ctx.day(file.created_at),
                sha256 => hash,
                embed => format!("![{alt}]({path})"),
                link => format!("[{alt}]({path})"),
                description => description_html,
                has_description => description.is_some(),
                may_edit => may_edit,
                add_description => ctx.link(&format!("/new?slug={path}")),
                uses => uses,
            }
        })
        .map_err(pages::template_error)?;
    Ok(pages::html_response(html, headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_becomes_a_page_name() {
        assert_eq!(stem_of("Ferris the Crab.PNG"), "ferris-the-crab");
        assert_eq!(stem_of("Филиан на стриме.png"), "filian-na-strime");
        assert_eq!(stem_of("???.png"), "file");
        assert_eq!(stem_of("no_extension"), "no-extension");
        assert_eq!(numbered("ferris", 1, "png"), "ferris.png");
        assert_eq!(numbered("ferris", 3, "png"), "ferris-3.png");
    }

    #[test]
    fn only_known_prefixes_and_clean_names_are_file_pages() {
        assert_eq!(split("image:ferris.png"), Some(("image", "ferris.png")));
        assert_eq!(split("file:notes.pdf"), Some(("file", "notes.pdf")));
        assert_eq!(split("template:x"), None);
        assert_eq!(split("image:Ferris.png"), None);
        assert_eq!(split("image:../x.png"), None);
        assert_eq!(split("image:noext"), None);
    }

    #[test]
    fn upload_keys_are_found_in_rendered_html() {
        let html = "<p><img src=\"/media/ab/abc.png\" alt=\"x\"> <a href=\"/media/cd/def.pdf\">d</a> <img src=\"/media/emotes/e.webp\"></p>";
        assert_eq!(
            keys_in_html(html),
            vec!["media/ab/abc.png", "media/cd/def.pdf"]
        );
    }
}
