//! Full text search over PostgreSQL, behind a backend trait.
//!
//! What is indexed is the article as a reader sees it: the rendered HTML,
//! templates expanded, turned back into plain text and cut at its headings
//! into pieces of a few thousand characters. Each piece has its own vector and
//! a hash of its text, so a save rewrites only the pieces that changed: one
//! edited paragraph in a 5 MB article costs one small vector, not five
//! megabytes of them. A query ranks pieces through a GIN index and builds its
//! snippet from the one best piece, and a result links to that section.
//!
//! Queries go through `websearch_to_tsquery`, which never raises on visitor
//! input. The text search configuration comes from the locale on both the
//! write and the read side, since a vector stemmed as `russian` does not match
//! a query stemmed as `english`.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::AppError;

/// One result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub slug: String,
    pub title: String,
    /// The section the best match is in, when it has a heading.
    pub section: Option<Section>,
    /// The matching text, escaped, with matches in `<mark>`. Built by
    /// [`clean_snippet`]; the only field safe to render unescaped.
    pub snippet: String,
}

/// A heading of an article: its text and the anchor the page gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub anchor: String,
}

/// One search request, backend independent.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub wiki_id: Uuid,
    pub text: &'a str,
    /// The language searched; results are limited to it.
    pub locale: &'a str,
    pub limit: i64,
}

#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn search(&self, request: Request<'_>) -> Result<Vec<Hit>, AppError>;
}

/// Maps a locale to a PostgreSQL text search configuration.
///
/// Returns a value from a closed list because it is cast to `::regconfig`,
/// which fails for an unknown name. Anything unrecognised, including CJK,
/// which PostgreSQL has no stemmer for, maps to `simple`.
pub fn regconfig_for(locale: &str) -> &'static str {
    let base = locale
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match base.as_str() {
        "ar" => "arabic",
        "ca" => "catalan",
        "da" => "danish",
        "de" => "german",
        "el" => "greek",
        "en" => "english",
        "es" => "spanish",
        "et" => "estonian",
        "eu" => "basque",
        "fi" => "finnish",
        "fr" => "french",
        "ga" => "irish",
        "hi" => "hindi",
        "hu" => "hungarian",
        "hy" => "armenian",
        "id" => "indonesian",
        "it" => "italian",
        "lt" => "lithuanian",
        "ne" => "nepali",
        "nl" => "dutch",
        "no" | "nb" | "nn" => "norwegian",
        "pt" => "portuguese",
        "ro" => "romanian",
        "ru" => "russian",
        "sr" => "serbian",
        "sv" => "swedish",
        "ta" => "tamil",
        "tr" => "turkish",
        "yi" => "yiddish",
        _ => "simple",
    }
}

/// Longest query run, in characters.
pub const QUERY_MAX: usize = 200;

/// Trims and caps a raw query; `None` when nothing is left.
pub fn normalize(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(QUERY_MAX).collect())
}

/// Match markers given to `ts_headline` as `chr(57344)` and `chr(57345)`.
const MATCH_OPEN: char = '\u{E000}';
const MATCH_CLOSE: char = '\u{E001}';

/// Turns a `ts_headline` result into safe HTML.
///
/// `ts_headline` escapes nothing, so the snippet is escaped as text first and
/// only then gains balanced `<mark>` tags.
pub fn clean_snippet(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 32);
    let mut open = false;
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut String| {
        out.push_str(&crate::html::escape(text));
        text.clear();
    };
    for c in raw.chars() {
        match c {
            MATCH_OPEN if !open => {
                flush(&mut text, &mut out);
                out.push_str("<mark>");
                open = true;
            }
            MATCH_CLOSE if open => {
                flush(&mut text, &mut out);
                out.push_str("</mark>");
                open = false;
            }
            MATCH_OPEN | MATCH_CLOSE => {}
            c => text.push(c),
        }
    }
    flush(&mut text, &mut out);
    if open {
        out.push_str("</mark>");
    }
    out
}

/// Matching pieces ranked per query, at most. A word on every page of a
/// huge wiki must not make one query rank the whole index.
const MATCHED_MAX: i64 = 5000;

/// Pages matched by title, at most, before ranking.
const TITLED_MAX: i64 = 500;

/// The PostgreSQL backend.
pub struct Postgres {
    db: sqlx::PgPool,
}

impl Postgres {
    pub fn new(db: sqlx::PgPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SearchBackend for Postgres {
    async fn search(&self, request: Request<'_>) -> Result<Vec<Hit>, AppError> {
        let config = regconfig_for(request.locale);
        // Pieces find stemmed words anywhere and rank the section; the page
        // vector ranks title and summary; trigram similarity on the title
        // catches typos and fragments. ts_headline runs last, on the one best
        // piece of each page that made the cut, never on a whole article.
        let rows = sqlx::query!(
            r#"
            -- $1 is bound as text and cast: sqlx has no mapping for regconfig.
            WITH q AS (
              SELECT websearch_to_tsquery($1::text::regconfig, $2) AS tsq
            ),
            matched AS (
              SELECT c.page_id, c.chunk_no, ts_rank_cd(c.vector, q.tsq) AS rank
              FROM search_chunks c, q
              WHERE c.wiki_id = $3 AND c.vector @@ q.tsq
              LIMIT $6
            ),
            best AS (
              SELECT DISTINCT ON (page_id) page_id, chunk_no, rank
              FROM matched ORDER BY page_id, rank DESC
            ),
            titled AS (
              SELECT p.id FROM pages p, q
              WHERE p.wiki_id = $3 AND p.namespace = 'main' AND p.deleted_at IS NULL
                AND COALESCE(p.locale, '') = $5
                AND (p.search_vector @@ q.tsq OR p.title % $2)
              LIMIT $7
            ),
            ranked AS (
              SELECT p.id, p.slug, p.title, p.updated_at, b.chunk_no,
                     ts_rank_cd(p.search_vector, q.tsq) * 10
                       + COALESCE(b.rank, 0) * 4
                       + similarity(p.title, $2) AS score
              FROM (SELECT page_id AS id FROM best UNION SELECT id FROM titled) cand
              JOIN pages p ON p.id = cand.id
              CROSS JOIN q
              LEFT JOIN best b ON b.page_id = p.id
              WHERE p.namespace = 'main' AND p.deleted_at IS NULL
                AND COALESCE(p.locale, '') = $5
              ORDER BY score DESC, p.updated_at DESC
              LIMIT $4
            )
            SELECT r.slug AS "slug!", r.title AS "title!", c.anchor AS "anchor?", c.heading AS "heading?",
                   ts_headline(
                     $1::text::regconfig, c.body, q.tsq,
                     'MaxWords=34, MinWords=14, ShortWord=3, MaxFragments=2,
                      FragmentDelimiter= … , StartSel=' || chr(57344) || ', StopSel=' || chr(57345)
                   ) AS "snippet?"
            FROM ranked r
            CROSS JOIN q
            -- The best piece, or the opening one for a match on the title alone.
            LEFT JOIN LATERAL (
              SELECT s.anchor, s.heading, s.body FROM search_chunks s
              WHERE s.page_id = r.id AND s.chunk_no = COALESCE(r.chunk_no, 1)
            ) c ON true
            ORDER BY r.score DESC, r.updated_at DESC
            "#,
            config,
            request.text,
            request.wiki_id,
            request.limit,
            request.locale,
            MATCHED_MAX,
            TITLED_MAX
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| Hit {
                slug: row.slug,
                title: row.title,
                section: match (row.anchor, row.heading) {
                    (Some(anchor), Some(heading)) if !anchor.is_empty() && !heading.is_empty() => {
                        Some(Section { heading, anchor })
                    }
                    _ => None,
                },
                snippet: clean_snippet(row.snippet.as_deref().unwrap_or("")),
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// From rendered HTML to indexed pieces
// ---------------------------------------------------------------------------

/// Text of one part of an article, between two headings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The heading's id on the page; empty before the first heading.
    pub anchor: String,
    pub heading: String,
    pub text: String,
}

/// The rendered article as plain text, cut at `<h1>` to `<h3>`. Tags go,
/// block ends become line breaks, entities are decoded, and the table of
/// contents is skipped since it only repeats the headings.
pub fn parts_from_html(html: &str) -> Vec<Part> {
    let mut parts = vec![Part {
        anchor: String::new(),
        heading: String::new(),
        text: String::new(),
    }];
    let mut in_heading = false;
    let mut skip_depth = 0usize;
    let mut rest = html;
    while !rest.is_empty() {
        let Some(open) = rest.find('<') else {
            push_text(&mut parts, in_heading, skip_depth, rest);
            break;
        };
        push_text(&mut parts, in_heading, skip_depth, &rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let tag = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("nav", false) if tag.contains("toc") => skip_depth += 1,
            ("nav", true) if skip_depth > 0 => skip_depth -= 1,
            ("h1" | "h2" | "h3", false) if skip_depth == 0 => {
                parts.push(Part {
                    anchor: attribute(tag, "id").unwrap_or_default(),
                    heading: String::new(),
                    text: String::new(),
                });
                in_heading = true;
            }
            ("h1" | "h2" | "h3", true) => in_heading = false,
            (
                "p" | "li" | "br" | "div" | "tr" | "dt" | "dd" | "pre" | "blockquote" | "aside"
                | "table" | "h4" | "h5" | "h6" | "summary" | "figcaption",
                _,
            ) => push_text(&mut parts, in_heading, skip_depth, "\n"),
            ("td" | "th", _) => push_text(&mut parts, in_heading, skip_depth, " "),
            _ => {}
        }
    }
    parts
        .into_iter()
        .map(|part| Part {
            anchor: part.anchor,
            heading: squash(&part.heading),
            text: squash(&part.text),
        })
        .filter(|part| !part.text.is_empty() || !part.heading.is_empty())
        .collect()
}

fn push_text(parts: &mut [Part], in_heading: bool, skip_depth: usize, raw: &str) {
    if skip_depth > 0 || raw.is_empty() {
        return;
    }
    let Some(part) = parts.last_mut() else {
        return;
    };
    let target = if in_heading {
        &mut part.heading
    } else {
        &mut part.text
    };
    decode_entities_into(raw, target);
}

/// The value of `name="..."` in a start tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')?;
    let mut value = String::new();
    decode_entities_into(&tag[start..start + end], &mut value);
    Some(value)
}

fn decode_entities_into(raw: &str, out: &mut String) {
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail[..tail.len().min(12)].find(';');
        let decoded = end.and_then(|end| {
            let entity = &tail[1..end];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
}

/// Runs of spaces become one space and runs of blank lines one line break.
fn squash(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&words.join(" "));
    }
    out
}

/// Characters in one indexed piece. Small enough that a snippet built from
/// it is cheap, large enough that a long article stays a few hundred rows.
pub const CHUNK_CHARS: usize = 6000;

/// One indexed piece of an article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub anchor: String,
    pub heading: String,
    pub body: String,
    /// Identifies the text and how it is stemmed, so an unchanged piece keeps
    /// its vector.
    pub hash: Vec<u8>,
}

/// The parts of an article as pieces of at most [`CHUNK_CHARS`], each hashed
/// with the search configuration that will stem it.
pub fn chunks(parts: &[Part], config: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    for part in parts {
        let pieces = split_for_index(&part.text, CHUNK_CHARS);
        for piece in pieces {
            let body = piece.text.trim().to_string();
            if body.is_empty() && !out.is_empty() && part.heading.is_empty() {
                continue;
            }
            let mut hasher = Sha256::new();
            for field in [config, &part.anchor, &part.heading, &body] {
                hasher.update((field.len() as u64).to_le_bytes());
                hasher.update(field.as_bytes());
            }
            out.push(Chunk {
                anchor: part.anchor.clone(),
                heading: part.heading.clone(),
                body,
                hash: hasher.finalize()[..16].to_vec(),
            });
        }
    }
    out
}

/// What one save changed in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexStats {
    /// Pieces whose vector was kept.
    pub kept: usize,
    /// Pieces stemmed and written.
    pub written: usize,
}

/// One page to index.
pub struct Document<'a> {
    pub page_id: Uuid,
    pub wiki_id: Uuid,
    pub locale: &'a str,
    pub title: &'a str,
    pub summary: Option<&'a str>,
    /// The page as rendered, templates expanded.
    pub html: &'a str,
}

/// Rebuilds one page's index inside the caller's transaction, so a save and
/// its index never disagree. The page vector holds the title and summary;
/// each piece of the body is a row of `search_chunks`, rewritten only when
/// its text or its stemming changed.
pub async fn index_page(
    conn: &mut sqlx::PgConnection,
    doc: &Document<'_>,
) -> Result<IndexStats, AppError> {
    let config = regconfig_for(doc.locale);
    sqlx::query!(
        r#"
        -- Every $2 is cast from ::text so PostgreSQL infers one type for it.
        UPDATE pages SET
          search_lang = $2::text,
          search_vector =
              setweight(to_tsvector($2::text::regconfig, $3), 'A')
           || setweight(to_tsvector($2::text::regconfig, coalesce($4, '')), 'B')
        WHERE id = $1
        "#,
        doc.page_id,
        config,
        doc.title,
        doc.summary
    )
    .execute(&mut *conn)
    .await?;

    let wanted = chunks(&parts_from_html(doc.html), config);
    let existing = sqlx::query!(
        "SELECT chunk_no, text_hash FROM search_chunks WHERE page_id = $1 FOR UPDATE",
        doc.page_id
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut by_hash: std::collections::HashMap<&[u8], Vec<i32>> = std::collections::HashMap::new();
    for row in &existing {
        by_hash
            .entry(row.text_hash.as_slice())
            .or_default()
            .push(row.chunk_no);
    }

    // Kept pieces are renumbered in place; the rest are stemmed and inserted.
    let (mut keep_old, mut keep_new) = (Vec::new(), Vec::new());
    let (mut add_no, mut add_anchor, mut add_heading, mut add_body, mut add_hash) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (i, chunk) in wanted.iter().enumerate() {
        let no = i as i32 + 1;
        match by_hash.get_mut(chunk.hash.as_slice()).and_then(Vec::pop) {
            Some(old) => {
                keep_old.push(-old - 1);
                keep_new.push(no);
            }
            None => {
                add_no.push(no);
                add_anchor.push(chunk.anchor.clone());
                add_heading.push(chunk.heading.clone());
                add_body.push(chunk.body.clone());
                add_hash.push(chunk.hash.clone());
            }
        }
    }

    // Out of the way first, so no renumbering collides with the key.
    sqlx::query!(
        "UPDATE search_chunks SET chunk_no = -chunk_no - 1 WHERE page_id = $1",
        doc.page_id
    )
    .execute(&mut *conn)
    .await?;
    if !keep_old.is_empty() {
        sqlx::query!(
            "UPDATE search_chunks c SET chunk_no = k.new_no
             FROM unnest($2::int[], $3::int[]) AS k(old_no, new_no)
             WHERE c.page_id = $1 AND c.chunk_no = k.old_no",
            doc.page_id,
            &keep_old,
            &keep_new
        )
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query!(
        "DELETE FROM search_chunks WHERE page_id = $1 AND chunk_no < 0",
        doc.page_id
    )
    .execute(&mut *conn)
    .await?;
    if !add_no.is_empty() {
        sqlx::query!(
            r#"
            INSERT INTO search_chunks (page_id, wiki_id, chunk_no, anchor, heading, body, text_hash, vector)
            SELECT $1, $2, c.no, c.anchor, c.heading, c.body, c.hash,
                   setweight(to_tsvector($3::text::regconfig, c.heading), 'A')
                || setweight(to_tsvector($3::text::regconfig, c.body), 'C')
            FROM unnest($4::int[], $5::text[], $6::text[], $7::text[], $8::bytea[])
                 AS c(no, anchor, heading, body, hash)
            "#,
            doc.page_id,
            doc.wiki_id,
            config,
            &add_no,
            &add_anchor,
            &add_heading,
            &add_body,
            &add_hash
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(IndexStats {
        kept: keep_new.len(),
        written: add_no.len(),
    })
}

/// One piece of a text cut by [`split_for_index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece<'a> {
    pub text: &'a str,
    /// Offset in characters from the start of the text.
    pub start_char: usize,
    pub len_chars: usize,
}

/// Cuts a text into pieces of at most `size` characters, on the last line
/// break or whitespace in the final tenth of each piece so no word is split.
/// Always returns at least one piece.
pub fn split_for_index(body: &str, size: usize) -> Vec<Piece<'_>> {
    let size = size.max(10);
    let mut pieces = Vec::new();
    let mut rest = body;
    let mut start_char = 0;
    loop {
        let Some((hard, _)) = rest.char_indices().nth(size) else {
            let len_chars = rest.chars().count();
            pieces.push(Piece {
                text: rest,
                start_char,
                len_chars,
            });
            return pieces;
        };
        let window = rest[..hard]
            .char_indices()
            .rev()
            .nth(size / 10)
            .map_or(0, |(i, _)| i);
        let tail = &rest[window..hard];
        let cut = tail
            .rfind('\n')
            .or_else(|| tail.rfind(char::is_whitespace))
            .map(|i| window + i)
            .filter(|&i| i > 0)
            .unwrap_or(hard);
        let text = &rest[..cut];
        let len_chars = text.chars().count();
        pieces.push(Piece {
            text,
            start_char,
            len_chars,
        });
        start_char += len_chars;
        rest = &rest[cut..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_is_cut_into_pieces_that_cover_all_of_it() {
        assert_eq!(split_for_index("", 100).len(), 1);
        let short = split_for_index("Filian", 100);
        assert_eq!(
            short,
            vec![Piece {
                text: "Filian",
                start_char: 0,
                len_chars: 6
            }]
        );

        let body = "Филиан светит снакерам. ".repeat(500) + "финал";
        let pieces = split_for_index(&body, 1000);
        assert!(pieces.len() > 1);
        let joined: String = pieces.iter().map(|p| p.text).collect();
        assert_eq!(joined, body);
        let mut at = 0;
        for piece in &pieces {
            assert_eq!(piece.start_char, at);
            assert_eq!(piece.len_chars, piece.text.chars().count());
            assert!(piece.len_chars <= 1000);
            at += piece.len_chars;
            let first = piece.text.chars().next().unwrap();
            assert!(piece.start_char == 0 || first.is_whitespace());
        }
        assert!(pieces.last().unwrap().text.ends_with("финал"));
    }

    #[test]
    fn a_body_without_spaces_is_still_cut() {
        let body = "я".repeat(2500);
        let pieces = split_for_index(&body, 1000);
        assert_eq!(
            pieces.iter().map(|p| p.len_chars).collect::<Vec<_>>(),
            vec![1000, 1000, 500]
        );
    }

    #[test]
    fn a_locale_picks_its_stemmer_and_a_region_suffix_is_ignored() {
        assert_eq!(regconfig_for("en"), "english");
        assert_eq!(regconfig_for("ru"), "russian");
        assert_eq!(regconfig_for("ru-RU"), "russian");
        assert_eq!(regconfig_for("ru_RU"), "russian");
        assert_eq!(regconfig_for("EN-gb"), "english");
        assert_eq!(regconfig_for("nb"), "norwegian");
    }

    #[test]
    fn an_unsupported_language_falls_back_instead_of_breaking_the_query() {
        for locale in [
            "ja",
            "ko",
            "zh-CN",
            "th",
            "he",
            "xx",
            "",
            "-",
            "not a locale",
        ] {
            assert_eq!(regconfig_for(locale), "simple", "{locale:?}");
        }
    }

    #[test]
    fn every_configuration_name_is_one_postgres_actually_ships() {
        // Every configuration shipped with the pinned PostgreSQL 18.
        const SHIPPED: &[&str] = &[
            "arabic",
            "armenian",
            "basque",
            "catalan",
            "danish",
            "dutch",
            "english",
            "estonian",
            "finnish",
            "french",
            "german",
            "greek",
            "hindi",
            "hungarian",
            "indonesian",
            "irish",
            "italian",
            "lithuanian",
            "nepali",
            "norwegian",
            "portuguese",
            "romanian",
            "russian",
            "serbian",
            "simple",
            "spanish",
            "swedish",
            "tamil",
            "turkish",
            "yiddish",
        ];
        for locale in [
            "ar", "hy", "eu", "ca", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hi", "hu",
            "id", "ga", "it", "lt", "ne", "no", "nb", "nn", "pt", "ro", "ru", "sr", "sv", "ta",
            "tr", "yi", "ja", "zz",
        ] {
            let config = regconfig_for(locale);
            assert!(SHIPPED.contains(&config), "{locale} -> {config}");
        }
    }

    #[test]
    fn blank_queries_never_reach_the_database() {
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("   "), None);
        assert_eq!(normalize("\t\n "), None);
        assert_eq!(normalize(" filian "), Some("filian".to_string()));
    }

    #[test]
    fn an_overlong_query_is_cut_on_a_character_boundary() {
        // Every character is two bytes, so byte slicing would panic.
        let cyrillic = "я".repeat(500);
        let capped = normalize(&cyrillic).expect("not blank");
        assert_eq!(capped.chars().count(), QUERY_MAX);
        assert!(capped.chars().all(|c| c == 'я'));

        let emoji = "🍪".repeat(300);
        assert_eq!(
            normalize(&emoji).expect("not blank").chars().count(),
            QUERY_MAX
        );
    }

    #[test]
    fn a_snippet_keeps_its_marks_and_escapes_everything_else() {
        let raw =
            "Audit page. \u{E000}zxqneedle\u{E001} <img src=x onerror=alert(document.domain)> text";
        let clean = clean_snippet(raw);
        assert!(clean.contains("<mark>zxqneedle</mark>"), "{clean}");
        assert!(
            clean.contains("&lt;img src=x onerror=alert(document.domain)&gt;"),
            "{clean}"
        );
    }

    #[test]
    fn only_the_markers_become_tags() {
        let clean = clean_snippet(
            "<mark onmouseover=x>\u{E000}a\u{E001}</mark> <script>alert(1)</script> 1 < 2 <a href=javascript:x>l</a>",
        );
        assert!(clean.contains("<mark>a</mark>"), "{clean}");
        let opened = clean.matches('<').count();
        let marks = clean.matches("<mark>").count() + clean.matches("</mark>").count();
        assert_eq!(opened, marks, "{clean}");
        assert!(clean.contains("1 &lt; 2"), "{clean}");
    }

    #[test]
    fn markers_in_the_text_cannot_unbalance_the_tags() {
        assert_eq!(
            clean_snippet("\u{E001}a\u{E000}\u{E000}b"),
            "a<mark>b</mark>"
        );
        let clean = clean_snippet("\u{E000}zxq\u{E001} tail <img src=x onerror=alert(1)");
        assert_eq!(
            clean,
            "<mark>zxq</mark> tail &lt;img src=x onerror=alert(1)"
        );
    }

    #[test]
    fn rendered_html_becomes_text_cut_at_its_headings() {
        let html = "<h1 id=\"filian\">Filian</h1>\n<p>Intro &amp; <strong>bold</strong> text.</p>\
                    <nav class=\"toc\"><ul><li><a href=\"#lore\">Lore</a></li></ul></nav>\
                    <h2 id=\"lore\">Lore <em>now</em></h2><p>Line one.</p><ul><li>a</li><li>b &lt;c&gt;</li></ul>\
                    <aside class=\"infobox\"><dl><div><dt>Debut</dt><dd>2021</dd></div></dl></aside>";
        let parts = parts_from_html(html);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[0].anchor, "filian");
        assert_eq!(parts[0].heading, "Filian");
        assert_eq!(parts[0].text, "Intro & bold text.");
        assert_eq!(parts[1].anchor, "lore");
        assert_eq!(parts[1].heading, "Lore now");
        assert_eq!(parts[1].text, "Line one.\na\nb <c>\nDebut\n2021");
    }

    #[test]
    fn text_before_the_first_heading_has_no_anchor() {
        let parts = parts_from_html("<p>Just text &#x2f; &#39;quoted&#39;</p>");
        assert_eq!(
            parts,
            vec![Part {
                anchor: String::new(),
                heading: String::new(),
                text: "Just text / 'quoted'".into()
            }]
        );
    }

    #[test]
    fn a_long_section_becomes_several_pieces_with_its_anchor() {
        let body = format!("<h2 id=\"big\">Big</h2><p>{}</p>", "word ".repeat(5000));
        let pieces = chunks(&parts_from_html(&body), "english");
        assert!(pieces.len() >= 4, "{}", pieces.len());
        assert!(
            pieces
                .iter()
                .all(|c| c.anchor == "big" && c.body.chars().count() <= CHUNK_CHARS)
        );
    }

    #[test]
    fn a_piece_hash_follows_its_text_and_its_stemming() {
        let parts = parts_from_html("<h2 id=\"a\">A</h2><p>one</p><h2 id=\"b\">B</h2><p>two</p>");
        let first = chunks(&parts, "english");
        let again = chunks(&parts, "english");
        assert_eq!(first, again);
        let edited = chunks(
            &parts_from_html("<h2 id=\"a\">A</h2><p>one</p><h2 id=\"b\">B</h2><p>three</p>"),
            "english",
        );
        assert_eq!(
            first[0].hash, edited[0].hash,
            "the untouched section keeps its vector"
        );
        assert_ne!(first[1].hash, edited[1].hash);
        assert_ne!(chunks(&parts, "russian")[0].hash, first[0].hash);
    }

    #[test]
    fn five_megabytes_of_article_cut_quickly() {
        let section = format!(
            "<h2 id=\"s\">S</h2><p>{}</p>",
            "Filian streams tonight. ".repeat(2000)
        );
        let html = section.repeat(110);
        assert!(html.len() > 5 * 1024 * 1024);
        // 0.1 s in a release build; the bound only catches a quadratic slip.
        let started = std::time::Instant::now();
        let pieces = chunks(&parts_from_html(&html), "english");
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        assert!(pieces.len() > 110);
    }
}
