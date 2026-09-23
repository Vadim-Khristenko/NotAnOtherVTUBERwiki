//! Search storage and querying, behind a backend trait.
//!
//! This lives in the core rather than in the web layer because writing the
//! index is a storage concern, not an HTTP one: the seeder needs it as much as
//! the save handler does, and a seeded page that is missing from search is a
//! wiki whose search box lies.
//!
//! PLAN.md decision D10 picked PostgreSQL full text first: it is already
//! running, it needs no second process, and on a wiki the size of a fan
//! community it is not the bottleneck. The trait exists so that swapping in
//! Meilisearch or Tantivy later is a new implementation rather than a rewrite.
//!
//! Two things here are easy to get wrong.
//!
//! **The query parser.** `to_tsquery` raises an error on input like `a & &`,
//! which arriving straight from a search box would be a 500 caused by a visitor
//! typing punctuation. `websearch_to_tsquery` never raises: it reads quotes as
//! phrases, `or` as alternation and a leading `-` as exclusion, and treats
//! anything else as words. A search box is exactly what it is for.
//!
//! **The stemmer has to match on both sides.** PostgreSQL stems when it writes
//! the tsvector, not when it reads it, so a vector built under `russian` and
//! queried under `english` matches almost nothing. The configuration comes from
//! the wiki locale in both directions, and `pages.search_lang` records which one
//! was used so a reindex can find rows that drifted after a locale change.

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::AppError;

/// One result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub slug: String,
    pub title: String,
    /// The matching part of the body with matched words wrapped in `<mark>`.
    /// Built by `ts_headline`, which escapes the surrounding text itself, so
    /// this is safe to render unescaped. It is the only field that is.
    pub snippet: String,
}

/// One search request, backend independent.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub wiki_id: Uuid,
    pub text: &'a str,
    /// The language searched: results are limited to it, and each backend
    /// maps it to what it needs, a PostgreSQL text search configuration here.
    pub locale: &'a str,
    pub limit: i64,
}

#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn search(&self, request: Request<'_>) -> Result<Vec<Hit>, AppError>;
}

/// Maps a locale to a PostgreSQL text search configuration.
///
/// The return type is `&'static str` from a closed list on purpose. These
/// values are interpolated into a `::regconfig` cast, and PostgreSQL raises an
/// error for a configuration that does not exist, so a locale read straight out
/// of the database must never reach that cast unchecked. Everything
/// unrecognised lands on `simple`, which does no stemming and no stop word
/// removal but tokenises any script.
///
/// `simple` is also the honest answer for Japanese, Korean and Chinese:
/// PostgreSQL ships no configuration for them, and they need a segmenter
/// (`pgroonga`, `zhparser`) rather than a stemmer. Whole-word matching still
/// works; relevance is worse than it could be.
pub fn regconfig_for(locale: &str) -> &'static str {
    // "ru-RU" and "ru" are the same language for stemming purposes.
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

/// Longest query we will run. Past this it is not a search, it is a paste.
pub const QUERY_MAX: usize = 200;

/// Trims and caps a raw query. `None` means there is nothing to search for,
/// which the route answers with the empty search page rather than a query.
pub fn normalize(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Cut on a character boundary. Slicing bytes would panic mid-codepoint on
    // a Cyrillic or Japanese query, which is most of the expected traffic.
    Some(trimmed.chars().take(QUERY_MAX).collect())
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
        // Two match paths, deliberately. The tsvector finds stemmed whole words
        // anywhere in the article, which is the main event. The trigram
        // similarity on the title catches a typo ("filan") and a fragment
        // ("ili"), neither of which a tsvector can match at all and both of
        // which a search box receives constantly.
        //
        // The score adds them with the full text side weighted far higher, so a
        // body match beats a fuzzy title resemblance. A title match beats both,
        // because `setweight` marked the title as rank A when the vector was
        // written.
        let rows = sqlx::query!(
            r#"
            -- $1 is bound as text and cast, never bound as regconfig. sqlx has
            -- no mapping for regconfig, and binding it directly is the error
            -- "no built-in mapping for type regconfig of param #1".
            WITH parsed AS (
              SELECT websearch_to_tsquery($1::text::regconfig, $2) AS tsq
            )
            SELECT p.slug,
                   p.title,
                   -- The snippet comes from where the words are: the head
                   -- when it matches, otherwise the best matching chunk.
                   ts_headline(
                     $1::text::regconfig,
                     CASE WHEN hit.start_char IS NOT NULL
                               AND NOT (p.search_vector @@ parsed.tsq)
                          THEN substr(r.body_md, hit.start_char + 1, hit.len_chars)
                          ELSE left(r.body_md, 200000) END,
                     parsed.tsq,
                     'MaxWords=34, MinWords=14, ShortWord=3, MaxFragments=2,
                      FragmentDelimiter= … , StartSel=<mark>, StopSel=</mark>'
                   ) AS "snippet!",
                   GREATEST(ts_rank_cd(p.search_vector, parsed.tsq), COALESCE(hit.rank, 0)) * 8
                     + similarity(p.title, $2) AS "score!"
            FROM pages p
            JOIN revisions r ON r.id = p.current_revision_id
            CROSS JOIN parsed
            -- Only long articles have chunks, and the primary key finds them,
            -- so a short page costs one empty index probe here.
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
              -- One language at a time: a search on the Russian side of a wiki
              -- finds Russian articles, and links to them in Russian.
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
                snippet: row.snippet,
            })
            .collect())
    }
}

/// Rebuilds `pages.search_vector` for one page.
///
/// Takes a connection rather than a pool so the save path can run it inside the
/// same transaction as the revision. A page is then never briefly saved but
/// unsearchable, and a failed index write rolls the save back rather than
/// leaving the two out of step.
///
/// The weights are the ranking policy: a hit in the title outranks one in the
/// summary, which outranks one in the body.
///
/// The whole body is indexed. An article may be 5 MB and a tsvector may not
/// pass 1 MB (PostgreSQL refuses the whole save with "string is too long for
/// tsvector"), so the page's own vector holds the title, the summary and the
/// first piece of the body, and any further pieces go to
/// `page_search_chunks`, one vector each. See `split_for_index`.
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
        -- Every use of $2 is annotated ::text. Writing it bare in the
        -- assignment and as ::regconfig in the casts made PostgreSQL deduce two
        -- different types for one parameter, which it refuses outright.
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
        // One statement for every piece: PostgreSQL builds the vectors, and
        // the text crosses the wire once.
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

/// Characters per indexed piece of a body. A tsvector costs at most about
/// three bytes per character of text (short unique words in a two byte
/// script), so 200 000 characters stays well under the 1 MB limit, and
/// nearly every article is a single piece.
pub const PIECE_CHARS: usize = 200_000;

/// One piece of a body, as indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece<'a> {
    pub text: &'a str,
    /// Offset in characters from the start of the body.
    pub start_char: usize,
    pub len_chars: usize,
}

/// Cuts a body into pieces of at most `size` characters for indexing. A cut
/// goes on the last line break in the final tenth of a piece, or else on the
/// last whitespace there, so a word is never split in two and lost to search.
/// Only a body with no whitespace at all is cut mid-word. Always at least one
/// piece, empty for an empty body.
pub fn split_for_index(body: &str, size: usize) -> Vec<Piece<'_>> {
    let size = size.max(10);
    let mut pieces = Vec::new();
    let mut rest = body;
    let mut start_char = 0;
    loop {
        // The byte offset of character `size`, or the whole remainder.
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

/// Rebuilds every live page's vectors in one wiki, or across the install when
/// `wiki_id` is `None`. Returns how many pages were touched.
///
/// Needed after a locale change, after a bulk import, and after any change to
/// the weights or the cut points in `index_page`. Every page goes through
/// `index_page` itself, so a reindex writes exactly what a save would: the
/// same stemmer for the page's own language, the same pieces. Pages are read
/// in batches, so a wiki of long articles never sits in memory whole, and each
/// batch is one transaction.
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
        // Nothing lost, nothing doubled, and the offsets line up.
        let joined: String = pieces.iter().map(|p| p.text).collect();
        assert_eq!(joined, body);
        let mut at = 0;
        for piece in &pieces {
            assert_eq!(piece.start_char, at);
            assert_eq!(piece.len_chars, piece.text.chars().count());
            assert!(piece.len_chars <= 1000);
            at += piece.len_chars;
            // Cut between words, never inside one.
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
        // PostgreSQL ships no configuration for these, and casting an unknown
        // name to regconfig raises an error. Landing on 'simple' keeps search
        // working with worse relevance, which is the honest trade.
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
        // The full list from `SELECT cfgname FROM pg_ts_config` on the pinned
        // PostgreSQL 18. A typo here would be a 500 on every search for that
        // one locale, which is the kind of bug that reaches production.
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
        // Byte slicing would panic here: every character is two bytes, so the
        // 200 byte mark lands inside one.
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
}
