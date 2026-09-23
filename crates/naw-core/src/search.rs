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
                   ts_headline(
                     $1::text::regconfig, r.body_md, parsed.tsq,
                     'MaxWords=34, MinWords=14, ShortWord=3, MaxFragments=2,
                      FragmentDelimiter= … , StartSel=<mark>, StopSel=</mark>'
                   ) AS "snippet!",
                   ts_rank_cd(p.search_vector, parsed.tsq) * 8
                     + similarity(p.title, $2) AS "score!"
            FROM pages p
            JOIN revisions r ON r.id = p.current_revision_id
            CROSS JOIN parsed
            WHERE p.wiki_id = $3
              AND p.namespace = 'main'
              AND p.deleted_at IS NULL
              -- One language at a time: a search on the Russian side of a wiki
              -- finds Russian articles, and links to them in Russian.
              AND COALESCE(p.locale, '') = $5
              AND (p.search_vector @@ parsed.tsq OR p.title % $2)
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
pub async fn index_page(
    conn: &mut sqlx::PgConnection,
    page_id: Uuid,
    locale: &str,
    title: &str,
    summary: Option<&str>,
    body_md: &str,
) -> Result<(), AppError> {
    let config = regconfig_for(locale);
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
        body_md
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Rebuilds every live page's vector in one wiki, or across the install when
/// `wiki_id` is `None`. Returns how many pages were touched.
///
/// Needed after a locale change, after a bulk import, and after any change to
/// the weights in `index_page`.
pub async fn reindex(db: &sqlx::PgPool, wiki_id: Option<Uuid>) -> Result<u64, AppError> {
    // The configuration is chosen per row from the owning wiki's locale, in
    // SQL, so one statement covers wikis in different languages. The CASE
    // mirrors `regconfig_for` for the languages this project actually serves;
    // anything else falls to 'simple' exactly as the Rust version does. The
    // lookup against pg_ts_config is the safety net that keeps a bad name from
    // raising an error mid-statement.
    let result = sqlx::query!(
        r#"
        UPDATE pages p SET
          search_lang = cfg.name,
          search_vector =
              setweight(to_tsvector(cfg.name::regconfig, p.title), 'A')
           || setweight(to_tsvector(cfg.name::regconfig, coalesce(r.summary, '')), 'B')
           || setweight(to_tsvector(cfg.name::regconfig, r.body_md), 'C')
        FROM revisions r, wikis w,
             LATERAL (
               SELECT COALESCE(
                 (SELECT c.cfgname::text FROM pg_ts_config c
                   WHERE c.cfgname::text = CASE split_part(lower(w.default_locale), '-', 1)
                     WHEN 'en' THEN 'english'
                     WHEN 'ru' THEN 'russian'
                     WHEN 'de' THEN 'german'
                     WHEN 'fr' THEN 'french'
                     WHEN 'es' THEN 'spanish'
                     WHEN 'it' THEN 'italian'
                     WHEN 'pt' THEN 'portuguese'
                     WHEN 'nl' THEN 'dutch'
                     WHEN 'tr' THEN 'turkish'
                     ELSE 'simple'
                   END),
                 'simple'
               ) AS name
             ) cfg
        WHERE r.id = p.current_revision_id
          AND w.id = p.wiki_id
          AND p.deleted_at IS NULL
          AND ($1::uuid IS NULL OR p.wiki_id = $1)
        "#,
        wiki_id
    )
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

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
