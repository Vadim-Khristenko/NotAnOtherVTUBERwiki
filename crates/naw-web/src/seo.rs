//! What search engines and link previews read.
//!
//! An article, a category and the front page carry a description, a
//! canonical address, their other languages (`hreflang`) and an OpenGraph
//! card. Every other page (editors, histories, diffs, system lists, the
//! account pages) says `noindex`: those are tools, and a search result that
//! lands in one helps nobody. `robots.txt` keeps crawlers out of the private
//! and expensive paths, and `sitemap.xml` lists every article and category.
//!
//! Absolute links start at the wiki's own domain (`Ctx::origin`), so an
//! alias never becomes the address search engines keep.

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages;
use crate::resolve::Ctx;

/// Characters of article text a description keeps. Search engines show
/// about this much.
const DESCRIPTION_CHARS: usize = 160;

/// A sitemap file holds at most this many addresses.
const SITEMAP_MAX: i64 = 50_000;

/// The engine's own square icon, for a card when the page has no picture.
const FALLBACK_IMAGE: &str = "/web-app-manifest-512x512.png";

/// `href` as an absolute address. Links built for a subdomain wiki are
/// absolute already.
pub(crate) fn absolute(ctx: &Ctx, href: &str) -> String {
    if href.starts_with("https://") || href.starts_with("http://") {
        href.to_string()
    } else {
        format!("{}{}", ctx.origin, pages::ascii_location(href))
    }
}

/// Elements whose paragraphs are not the article's opening: an infobox, a
/// notice, a table, a quote, the contents.
const CONTAINERS: &[&str] = &[
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "figure",
    "footer",
    "header",
    "nav",
    "ol",
    "section",
    "table",
    "ul",
];

/// The tag at the start of `rest` (just after `<`): its lowercase name and
/// whether it closes.
fn tag_name(rest: &str) -> (String, bool) {
    let (closing, body) = match rest.strip_prefix('/') {
        Some(body) => (true, body),
        None => (false, rest),
    };
    let name: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    (name, closing)
}

/// The first paragraph of rendered article text as plain text, cut to about
/// [`DESCRIPTION_CHARS`]. Paragraphs inside an infobox, a notice or a list
/// are skipped, and so are footnote marks and emotes.
pub(crate) fn description(html: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut at = 0usize;
    while let Some(open) = html[at..].find('<').map(|i| at + i) {
        let rest = &html[open + 1..];
        let end = rest.find('>').map(|i| open + 1 + i)?;
        let (name, closing) = tag_name(rest);
        let self_closing = html[..end].ends_with('/');
        if CONTAINERS.contains(&name.as_str()) && !self_closing {
            if closing {
                depth = depth.saturating_sub(1);
            } else {
                depth += 1;
            }
        } else if name == "p" && !closing && depth == 0 {
            let close = html[end..].find("</p>").map(|i| end + i)?;
            let text = plain_text(&html[end + 1..close]);
            if text.chars().count() >= 20 {
                return Some(cut(&text, DESCRIPTION_CHARS));
            }
            at = close + 4;
            continue;
        }
        at = end + 1;
    }
    None
}

/// Text without tags, footnote marks or pictures, entities decoded and
/// whitespace collapsed.
fn plain_text(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    let mut rest = fragment;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(end) = after.find('>') else {
            rest = "";
            break;
        };
        let (name, closing) = tag_name(after);
        rest = &after[end + 1..];
        // A footnote mark is a number that means nothing out of place.
        if name == "sup" && !closing {
            rest = match rest.find("</sup>") {
                Some(i) => &rest[i + 6..],
                None => "",
            };
        }
    }
    out.push_str(rest);
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The entities the renderer and the sanitizer write.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after.find(';').filter(|&i| i <= 10).and_then(|i| {
            let name = &after[..i];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((ch, i))
        });
        match decoded {
            Some((ch, len)) => {
                out.push(ch);
                rest = &after[len + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `text` cut at a word before `max` characters, with an ellipsis when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    let head = match head.rfind(' ') {
        Some(space) if space > max / 2 => &head[..space],
        _ => head.as_str(),
    };
    format!(
        "{}…",
        head.trim_end_matches(|c: char| c.is_whitespace() || ",;:-(".contains(c))
    )
}

/// The first picture of the article that is not an emote: what a link
/// preview shows.
pub(crate) fn first_image(html: &str) -> Option<String> {
    let mut at = 0usize;
    while let Some(open) = html[at..].find("<img").map(|i| at + i) {
        let end = html[open..].find('>').map(|i| open + i)?;
        let tag = &html[open..end];
        at = end;
        if attribute(tag, "class").is_some_and(|c| c.split_whitespace().any(|w| w == "emote")) {
            continue;
        }
        if let Some(src) = attribute(tag, "src")
            && (src.starts_with('/') || src.starts_with("https://"))
            && !src.starts_with("//")
        {
            return Some(decode_entities(src));
        }
    }
    None
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let len = tag[start..].find('"')?;
    Some(&tag[start..start + len])
}

/// OpenGraph spells a language with its region.
fn og_locale(code: &str) -> String {
    match code {
        "en" => "en_US".to_string(),
        "ru" => "ru_RU".to_string(),
        "uk" => "uk_UA".to_string(),
        "ja" => "ja_JP".to_string(),
        other if other.len() == 2 => format!("{other}_{}", other.to_ascii_uppercase()),
        other => other.to_string(),
    }
}

/// The languages the page at `path` can be read in now.
pub(crate) async fn live_languages(
    db: &sqlx::PgPool,
    ctx: &Ctx,
    path: &str,
) -> Result<Vec<String>, AppError> {
    let (namespace, slug) = pages::split_path(path);
    Ok(sqlx::query_scalar!(
        r#"SELECT COALESCE(locale, '') AS "locale!" FROM pages
           WHERE wiki_id = $1 AND namespace = ($2::text)::page_namespace AND slug = $3
             AND deleted_at IS NULL AND current_revision_id IS NOT NULL
           ORDER BY (COALESCE(locale, '') = $4) DESC, locale"#,
        ctx.wiki.id,
        namespace,
        slug,
        ctx.wiki.default_locale
    )
    .fetch_all(db)
    .await?)
}

/// A page search engines may keep, as its `<head>` describes it.
pub(crate) struct Card<'a> {
    pub title: &'a str,
    /// Plain text; the wiki's own line stands in when there is none.
    pub description: Option<String>,
    /// The address this page is kept under: `/filian`, `/ru/filian`, `/`.
    pub href: String,
    pub image: Option<String>,
    /// An article, rather than the front page or a list.
    pub article: bool,
    /// The page's other languages: (code, href), this one included.
    pub languages: Vec<(String, String)>,
}

/// What the layout writes into `<head>` for a page search engines may keep.
pub(crate) fn head(ctx: &Ctx, card: Card<'_>) -> minijinja::Value {
    let description = card
        .description
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| ctx.t_with("seo.fallback", &[("wiki", &ctx.wiki.name)]));
    let picture = card.image.as_deref().map(|src| absolute(ctx, src));
    let alternates: Vec<minijinja::Value> = if card.languages.len() > 1 {
        card.languages
            .iter()
            .map(|(code, href)| minijinja::context! { code => code, href => absolute(ctx, href) })
            .collect()
    } else {
        Vec::new()
    };
    let x_default = card
        .languages
        .iter()
        .find(|(code, _)| *code == ctx.wiki.default_locale)
        .filter(|_| card.languages.len() > 1)
        .map(|(_, href)| absolute(ctx, href));
    minijinja::context! {
        title => card.title,
        description => description,
        canonical => absolute(ctx, &card.href),
        large_image => picture.is_some(),
        image => picture.unwrap_or_else(|| absolute(ctx, FALLBACK_IMAGE)),
        og_type => if card.article { "article" } else { "website" },
        og_locale => og_locale(&ctx.content_locale),
        alternates => alternates,
        x_default => x_default,
    }
}

/// The card of the article or category at `path` in the reader's language.
pub(crate) async fn card_for(
    state: &AppState,
    ctx: &Ctx,
    path: &str,
    title: &str,
    body_html: &str,
    href: String,
    article: bool,
) -> Result<minijinja::Value, AppError> {
    let languages = live_languages(&state.db, ctx, path)
        .await?
        .into_iter()
        .map(|code| {
            let href = ctx.link_for(&code, &format!("/{path}"));
            (code, href)
        })
        .collect();
    Ok(head(
        ctx,
        Card {
            title,
            description: description(body_html),
            href,
            image: first_image(body_html),
            article,
            languages,
        },
    ))
}

/// GET /robots.txt
pub async fn robots_txt(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let body = format!(
        "# {name}\n\
         User-agent: *\n\
         Disallow: /admin\n\
         Disallow: /auth/\n\
         Disallow: /login\n\
         Disallow: /logout\n\
         Disallow: /settings\n\
         Disallow: /drafts\n\
         Disallow: /notifications\n\
         Disallow: /watchlist\n\
         Disallow: /search\n\
         Disallow: /preview\n\
         Disallow: /new\n\
         Disallow: /media/import\n\
         Disallow: /*/edit$\n\
         Disallow: /*/history$\n\
         Disallow: /*/history?\n\
         Disallow: /*/diff?\n\
         Disallow: /*/rev/\n\
         Disallow: /*/report?\n\
         Disallow: /*/translate\n\
         Allow: /\n\
         \n\
         Sitemap: {origin}/sitemap.xml\n",
        name = ctx.wiki.name.replace(['\r', '\n'], " "),
        origin = ctx.origin
    );
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response())
}

/// One language of a page in the sitemap, and when it last changed.
type Version = (String, chrono::DateTime<chrono::Utc>);

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// GET /sitemap.xml: the front page, every article in every language with
/// its other languages beside it, and every category in use.
pub async fn sitemap(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::resolve::required(&state, &headers, user.as_ref()).await?);
    let rows = sqlx::query!(
        r#"SELECT slug, COALESCE(locale, '') AS "locale!", updated_at FROM pages
           WHERE wiki_id = $1 AND namespace = 'main' AND deleted_at IS NULL
             AND current_revision_id IS NOT NULL
           ORDER BY slug, (COALESCE(locale, '') = $2) DESC, locale
           LIMIT $3"#,
        ctx.wiki.id,
        ctx.wiki.default_locale,
        SITEMAP_MAX
    )
    .fetch_all(&state.db)
    .await?;
    let home = crate::landing::home_slug(&ctx);
    // One entry per language, each naming every language of its page.
    let mut groups: Vec<(String, Vec<Version>)> = Vec::new();
    for row in rows {
        match groups.last_mut() {
            Some((slug, versions)) if *slug == row.slug => {
                versions.push((row.locale, row.updated_at))
            }
            _ => groups.push((row.slug, vec![(row.locale, row.updated_at)])),
        }
    }
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" xmlns:xhtml=\"http://www.w3.org/1999/xhtml\">\n",
    );
    let path_of = |slug: &str| {
        if slug == home {
            "/".to_string()
        } else {
            format!("/{slug}")
        }
    };
    for (slug, versions) in &groups {
        let path = path_of(slug);
        let links: Vec<(String, String)> = versions
            .iter()
            .map(|(code, _)| (code.clone(), absolute(&ctx, &ctx.link_for(code, &path))))
            .collect();
        for (code, at) in versions {
            let loc = absolute(&ctx, &ctx.link_for(code, &path));
            xml.push_str(&format!(
                "<url><loc>{}</loc><lastmod>{}</lastmod>",
                xml_escape(&loc),
                at.format("%Y-%m-%d")
            ));
            if links.len() > 1 {
                for (other, href) in &links {
                    xml.push_str(&format!(
                        "<xhtml:link rel=\"alternate\" hreflang=\"{}\" href=\"{}\"/>",
                        xml_escape(other),
                        xml_escape(href)
                    ));
                }
            }
            xml.push_str("</url>\n");
        }
    }
    let categories = sqlx::query!(
        r#"SELECT pc.category, max(p.updated_at) AS "at!"
           FROM page_categories pc JOIN pages p ON p.id = pc.page_id
           WHERE pc.wiki_id = $1 AND p.deleted_at IS NULL AND p.current_revision_id IS NOT NULL
           GROUP BY pc.category ORDER BY pc.category LIMIT $2"#,
        ctx.wiki.id,
        SITEMAP_MAX
    )
    .fetch_all(&state.db)
    .await?;
    for row in categories {
        let loc = absolute(&ctx, &crate::categories::href(&ctx, &row.category));
        xml.push_str(&format!(
            "<url><loc>{}</loc><lastmod>{}</lastmod></url>\n",
            xml_escape(&loc),
            row.at.format("%Y-%m-%d")
        ));
    }
    xml.push_str("</urlset>\n");
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        xml,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = r##"<h1 id="filian">Filian</h1>
<aside class="infobox"><p class="infobox-title">Filian</p><p><em>An independent VTuber who streams on Twitch</em></p>
<dl class="infobox-rows"><div><dt>Debut</dt><dd>2021</dd></div></dl></aside>
<p><strong>Filian</strong> is an independent English-speaking VTuber &amp; streamer.<sup class="footnote-reference" id="fnref-a"><a href="#fn-a">1</a></sup> She started in 2021. <img class="emote" src="/media/emotes/x.webp" alt=":fillySmile:" /></p>
<p>Second paragraph.</p>"##;

    #[test]
    fn the_description_is_the_opening_paragraph_not_the_infobox() {
        assert_eq!(
            description(ARTICLE).as_deref(),
            Some(
                "Filian is an independent English-speaking VTuber & streamer. She started in 2021."
            )
        );
    }

    #[test]
    fn a_notice_before_the_text_is_skipped_and_short_lines_too() {
        let html = r#"<div class="wiki-notice"><p>This page must be updated, and here is why it says so.</p></div>
<p>Hi.</p><p>Друзья Филиан &#8212; это зрители её стримов, и их называют снэкерсами.</p>"#;
        assert_eq!(
            description(html).as_deref(),
            Some("Друзья Филиан \u{2014} это зрители её стримов, и их называют снэкерсами.")
        );
        assert_eq!(description("<ul><li><p>only a list</p></li></ul>"), None);
    }

    #[test]
    fn a_long_opening_is_cut_at_a_word() {
        let long = format!("<p>{}</p>", "word ".repeat(100));
        let cut = description(&long).expect("text");
        assert!(cut.ends_with("word…"), "{cut}");
        assert!(cut.chars().count() <= DESCRIPTION_CHARS + 1);
    }

    #[test]
    fn the_card_picture_is_never_an_emote() {
        assert_eq!(first_image(ARTICLE), None);
        let html = r#"<p><img class="emote" src="/media/emotes/a.webp" /><img src="/media/ab/cd.png" alt="Filian" /></p>"#;
        assert_eq!(first_image(html).as_deref(), Some("/media/ab/cd.png"));
        assert_eq!(first_image(r#"<img src="//evil.example/x.png" />"#), None);
    }

    #[test]
    fn languages_get_their_region() {
        assert_eq!(og_locale("ru"), "ru_RU");
        assert_eq!(og_locale("de"), "de_DE");
    }
}
