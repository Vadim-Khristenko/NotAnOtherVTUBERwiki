//! Full text search over PostgreSQL, behind a backend trait.
//!
//! Queries go through `websearch_to_tsquery`, which never raises on visitor
//! input. The text search configuration comes from the locale on both the
//! write and the read side, since a vector stemmed as `russian` does not match
//! a query stemmed as `english`.

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::AppError;

/// One result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub slug: String,
    pub title: String,
    /// The matching part of the body, escaped, with matches in `<mark>`. Built by
    /// [`clean_snippet`]; the only field safe to render unescaped.
    pub snippet: String,
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
/// `ts_headline` escapes nothing and runs over raw Markdown, so the snippet
/// is escaped as text first and only then gains balanced `<mark>` tags.
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
        // Full text finds stemmed words anywhere; trigram similarity on the title
        // catches typos and fragments. Full text weighs far more in the score.
        let rows = sqlx::query!(
            r#"
            -- $1 is bound as text and cast: sqlx has no mapping for regconfig.
            WITH parsed AS (
              SELECT websearch_to_tsquery($1::text::regconfig, $2) AS tsq
            )
            SELECT p.slug,
                   p.title,
                   -- From the head when it matches, otherwise from the best chunk.
                   ts_headline(
                     $1::text::regconfig,
                     CASE WHEN hit.start_char IS NOT NULL
                               AND NOT (p.search_vector @@ parsed.tsq)
                          THEN substr(r.body_md, hit.start_char + 1, hit.len_chars)
                          ELSE left(r.body_md, 200000) END,
                     parsed.tsq,
                     'MaxWords=34, MinWords=14, ShortWord=3, MaxFragments=2,
                      FragmentDelimiter= … , StartSel=' || chr(57344) || ', StopSel=' || chr(57345)
                   ) AS "snippet!",
                   GREATEST(ts_rank_cd(p.search_vector, parsed.tsq), COALESCE(hit.rank, 0)) * 8
                     + similarity(p.title, $2) AS "score!"
            FROM pages p
            JOIN revisions r ON r.id = p.current_revision_id
            CROSS JOIN parsed
            LEFT JOIN LATERAL (
              SELECT c.start_char, c.len_chars, ts_rank_cd(c.vector, parsed.tsq) AS rank
              FROM page_search_chunks c
              WHERE c.page_id = p.id AND c.vector @@ parsed.tsq
              ORDER BY rank DESC
              LIMIT 1
            ) hit ON true
            WHERE p.wiki_id = $3
              AND p.namespace = 'main'
              AND p.deleted_at IS NULL
              AND COALESCE(p.locale, '') = $5
              AND (p.search_vector @@ parsed.tsq OR p.title % $2 OR hit.start_char IS NOT NULL)
            ORDER BY "score!" DESC, p.updated_at DESC
            LIMIT $4
            "#,
            config,
            request.text,
            request.wiki_id,
            request.limit,
            request.locale
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| Hit {
                slug: row.slug,
                title: row.title,
                snippet: clean_snippet(&row.snippet),
            })
            .collect())
    }
}

/// Rebuilds the search vectors of one page, inside the caller's transaction
/// so a save and its index never disagree.
///
/// Weights rank title over summary over body. A tsvector may not pass 1 MB,
/// so the page vector holds the title, the summary and the first piece of the
/// body, and further pieces go to `page_search_chunks` (see [`split_for_index`]).
pub async fn index_page(
    conn: &mut sqlx::PgConnection,
    page_id: Uuid,
    locale: &str,
    title: &str,
    summary: Option<&str>,
    body_md: &str,
) -> Result<(), AppError> {
    let config = regconfig_for(locale);
    let pieces = split_for_index(body_md, PIECE_CHARS);
    let head = pieces.first().map_or("", |piece| piece.text);
    sqlx::query!(
        r#"
        -- Every $2 is cast from ::text so PostgreSQL infers one type for it.
        UPDATE pages SET
          search_lang = $2::text,
          search_vector =
              setweight(to_tsvector($2::text::regconfig, $3), 'A')
           || setweight(to_tsvector($2::text::regconfig, coalesce($4, '')), 'B')
           || setweight(to_tsvector($2::text::regconfig, $5), 'C')
        WHERE id = $1
        "#,
        page_id,
        config,
        title,
        summary,
        head
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!("DELETE FROM page_search_chunks WHERE page_id = $1", page_id)
        .execute(&mut *conn)
        .await?;
    if pieces.len() > 1 {
        let rest = &pieces[1..];
        let numbers: Vec<i32> = (1..=rest.len() as i32).collect();
        let starts: Vec<i32> = rest.iter().map(|p| p.start_char as i32).collect();
        let lens: Vec<i32> = rest.iter().map(|p| p.len_chars as i32).collect();
        let texts: Vec<String> = rest.iter().map(|p| p.text.to_string()).collect();
        sqlx::query!(
            r#"
            INSERT INTO page_search_chunks (page_id, chunk_no, start_char, len_chars, vector)
            SELECT $1, c.no, c.start_char, c.len_chars,
                   setweight(to_tsvector($2::text::regconfig, c.body), 'C')
            FROM unnest($3::int[], $4::int[], $5::int[], $6::text[])
                 AS c(no, start_char, len_chars, body)
            "#,
            page_id,
            config,
            &numbers,
            &starts,
            &lens,
            &texts
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Characters per indexed piece. At most about three bytes of tsvector per
/// character, so a piece stays well under the 1 MB limit.
pub const PIECE_CHARS: usize = 200_000;

/// One indexed piece of a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece<'a> {
    pub text: &'a str,
    /// Offset in characters from the start of the body.
    pub start_char: usize,
    pub len_chars: usize,
}

/// Cuts a body into pieces of at most `size` characters, on the last line
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

/// Rebuilds every live page's vectors in one wiki, or everywhere when
/// `wiki_id` is `None`, through [`index_page`] in batches of one transaction
/// each. Returns how many pages were indexed.
pub async fn reindex(db: &sqlx::PgPool, wiki_id: Option<Uuid>) -> Result<u64, AppError> {
    const BATCH: usize = 50;
    let ids = sqlx::query_scalar!(
        "SELECT id FROM pages
         WHERE deleted_at IS NULL AND current_revision_id IS NOT NULL
           AND ($1::uuid IS NULL OR wiki_id = $1)
         ORDER BY id",
        wiki_id
    )
    .fetch_all(db)
    .await?;
    let mut done = 0;
    for batch in ids.chunks(BATCH) {
        let mut tx = db.begin().await?;
        let pages = sqlx::query!(
            r#"
            SELECT p.id, COALESCE(NULLIF(p.locale, ''), w.default_locale) AS "locale!",
                   p.title, r.summary, r.body_md
            FROM pages p
            JOIN revisions r ON r.id = p.current_revision_id
            JOIN wikis w ON w.id = p.wiki_id
            WHERE p.id = ANY($1)
            "#,
            batch
        )
        .fetch_all(&mut *tx)
        .await?;
        for page in pages {
            index_page(
                &mut tx,
                page.id,
                &page.locale,
                &page.title,
                page.summary.as_deref(),
                &page.body_md,
            )
            .await?;
            done += 1;
        }
        tx.commit().await?;
    }
    Ok(done)
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
}
