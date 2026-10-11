//! `naw import`: a MediaWiki or FANDOM XML dump, converted and, on request,
//! written into a wiki.
//!
//! Without `--write` this is a dry run: every page's latest text is
//! converted, the Markdown can be written to a folder to read through, and
//! a report says what carried over, which templates and files the pages
//! need, and what could not be converted.
//!
//! With `--write --wiki SLUG`, each article, template and category comes in
//! with its whole history: every revision converted, dated as it was, and
//! credited to its author's name on the source wiki (see migration 0038).
//! A page that already exists here is left alone, never overwritten.
//! Redirects become redirects. The wiki is reindexed at the end, so
//! categories, links and search see the new pages.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use uuid::Uuid;

use naw_wikitext::dump::{Author, Dump, Page};
use naw_wikitext::{Options, Report, convert};

const USAGE: &str = "usage: naw import <dump.xml> [--out DIR] [--report FILE] [--write --wiki SLUG]

  Converts a MediaWiki or FANDOM XML dump (Special:Export, the API's export,
  or a database dump) and reports what carries over.

  --out DIR      write each page's Markdown to DIR, one file per page
  --report FILE  write the report to FILE as well as printing it
  --write        bring the pages into the wiki named by --wiki, with their
                 history; pages that already exist there are left alone
  --wiki SLUG    the wiki to write into";

/// Namespaces that become pages here: articles, templates and categories.
const MAIN: i32 = 0;
const FILE: i32 = 6;
const TEMPLATE: i32 = 10;
const CATEGORY: i32 = 14;

pub async fn command(args: Vec<String>) -> ExitCode {
    let mut dump_path = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut report_path: Option<PathBuf> = None;
    let mut wiki: Option<String> = None;
    let mut write = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out_dir = args.get(i).map(PathBuf::from);
            }
            "--report" => {
                i += 1;
                report_path = args.get(i).map(PathBuf::from);
            }
            "--wiki" => {
                i += 1;
                wiki = args.get(i).cloned();
            }
            "--write" => write = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with("--") => {
                eprintln!("unknown option {other}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
            other => dump_path = Some(PathBuf::from(other)),
        }
        i += 1;
    }
    let Some(dump_path) = dump_path else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let writer = match (write, wiki) {
        (false, _) => None,
        (true, None) => {
            eprintln!("--write needs --wiki\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
        (true, Some(slug)) => match Writer::open(&slug).await {
            Ok(writer) => Some(writer),
            Err(err) => {
                eprintln!("import: {err}");
                return ExitCode::FAILURE;
            }
        },
    };
    match run(
        &dump_path,
        out_dir.as_deref(),
        report_path.as_deref(),
        writer,
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("import: {err}");
            ExitCode::FAILURE
        }
    }
}

/// What the whole dump came to.
#[derive(Default)]
struct Totals {
    report: Report,
    /// Pages read per namespace number.
    read: BTreeMap<i32, usize>,
    converted: usize,
    /// Pages with nothing left out.
    clean: usize,
    redirects: usize,
    revisions: usize,
    /// Pages whose model is not wikitext (CSS, JavaScript, Lua).
    code_pages: Vec<String>,
    /// Addresses the dump has pages for.
    have_pages: BTreeSet<String>,
    have_templates: BTreeSet<String>,
    /// The pages that left the most out, with how much.
    worst: Vec<(usize, String)>,
    /// With `--write`: pages and redirects written, and pages already there.
    written: usize,
    written_revisions: usize,
    written_redirects: usize,
    existing: Vec<String>,
}

async fn run(
    dump_path: &Path,
    out_dir: Option<&Path>,
    report_path: Option<&Path>,
    writer: Option<Writer>,
) -> Result<(), String> {
    let open = || {
        std::fs::File::open(dump_path)
            .map(|f| Dump::new(BufReader::new(f)))
            .map_err(|err| format!("{}: {err}", dump_path.display()))
    };
    // First pass: which template parameters take a picture, so the calls
    // in the articles can name the files the engine's way.
    let mut image_params: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut templates = open()?;
    while let Some(page) = templates.next_page().map_err(|err| err.to_string())? {
        if page.ns != TEMPLATE {
            continue;
        }
        let (Some(slug), Some(latest)) = (template_slug(page.bare_title()), page.revisions.last())
        else {
            continue;
        };
        let params = naw_wikitext::image_params(&latest.text);
        if !params.is_empty() {
            image_params.insert(slug, params);
        }
    }
    let mut dump = open()?;
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    }
    let mut totals = Totals::default();
    while let Some(page) = dump.next_page().map_err(|err| err.to_string())? {
        *totals.read.entry(page.ns).or_default() += 1;
        totals.revisions += page.revisions.len();
        if !matches!(page.ns, MAIN | TEMPLATE | CATEGORY) {
            continue;
        }
        let Some(latest) = page.revisions.last() else {
            continue;
        };
        if !latest.model.is_empty() && latest.model != "wikitext" {
            totals.code_pages.push(page.title.clone());
            continue;
        }
        let Some(path) = path_of(&page) else {
            let entry = totals
                .report
                .unsupported
                .entry("title with no address".into())
                .or_default();
            if entry.count == 0 {
                entry.example = page.title.clone();
            }
            entry.count += 1;
            continue;
        };
        let options = Options {
            title: page.bare_title().to_string(),
            category_names: dump
                .site
                .namespace(CATEGORY)
                .map(str::to_string)
                .into_iter()
                .collect(),
            file_names: dump
                .site
                .namespace(FILE)
                .map(str::to_string)
                .into_iter()
                .collect(),
            template: page.ns == TEMPLATE,
            image_params: image_params.clone(),
        };
        let converted = convert(&latest.text, &options);
        if page.ns == TEMPLATE {
            totals
                .have_templates
                .insert(path.trim_start_matches("template:").to_string());
        } else {
            totals.have_pages.insert(path.clone());
        }
        let redirect = converted.redirect.clone().or_else(|| page.redirect.clone());
        if let Some(target) = redirect {
            totals.redirects += 1;
            if let Some(writer) = &writer {
                let to = Page {
                    title: target,
                    ns: page.ns,
                    ..Page::default()
                };
                if let Some(to_path) = path_of(&to)
                    && writer.redirect(&path, &to_path).await?
                {
                    totals.written_redirects += 1;
                }
            }
            continue;
        }
        totals.converted += 1;
        let missed: usize = converted.report.unsupported.values().map(|u| u.count).sum();
        if missed == 0 {
            totals.clean += 1;
        } else {
            totals.worst.push((missed, page.title.clone()));
        }
        totals.report.merge(&converted.report);
        if let Some(dir) = out_dir {
            let file = dir.join(format!("{}.md", path.replace(':', "__")));
            std::fs::write(&file, &converted.markdown)
                .map_err(|err| format!("{}: {err}", file.display()))?;
        }
        if let Some(writer) = &writer {
            match writer.page(&page, &path, &options, &dump.site.name).await? {
                Some(revisions) => {
                    totals.written += 1;
                    totals.written_revisions += revisions;
                }
                None => totals.existing.push(path),
            }
        }
    }
    if let Some(writer) = &writer {
        let count = naw_web::indexing::reindex(&writer.pool, &writer.limits, Some(writer.wiki_id))
            .await
            .map_err(|err| format!("reindex: {err}"))?;
        eprintln!("reindexed {count} page(s)");
    }
    let report = render(&dump.site.name, &totals, writer.is_some());
    println!("{report}");
    if let Some(path) = report_path {
        std::fs::write(path, &report).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(())
}

/// Writes pages into one wiki.
struct Writer {
    pool: sqlx::PgPool,
    limits: naw_core::limits::Limits,
    wiki_id: Uuid,
    locale: String,
}

impl Writer {
    async fn open(slug: &str) -> Result<Writer, String> {
        let limits = crate::load_config()?.limits;
        let pool = crate::pool_or_exit()
            .await
            .map_err(|_| "no database".to_string())?;
        let row = sqlx::query!("SELECT id, default_locale FROM wikis WHERE slug = $1", slug)
            .fetch_optional(&pool)
            .await
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("no wiki with slug {slug}"))?;
        Ok(Writer {
            pool,
            limits,
            wiki_id: row.id,
            locale: row.default_locale,
        })
    }

    /// Writes a page with every revision the dump holds for it, or returns
    /// `None` when a page at that address already exists.
    async fn page(
        &self,
        page: &Page,
        path: &str,
        options: &Options,
        site: &str,
    ) -> Result<Option<usize>, String> {
        let (namespace, slug) = split(path);
        let mut tx = self.pool.begin().await.map_err(|err| err.to_string())?;
        let page_id = Uuid::new_v4();
        let first = page
            .revisions
            .first()
            .and_then(|r| timestamp(&r.timestamp))
            .unwrap_or_else(chrono::Utc::now);
        let inserted = sqlx::query!(
            "INSERT INTO pages (id, wiki_id, namespace, slug, title, locale, created_at)
             VALUES ($1, $2, ($3::text)::page_namespace, $4, $5, $6, $7)
             ON CONFLICT DO NOTHING",
            page_id,
            self.wiki_id,
            namespace,
            slug,
            page.bare_title(),
            self.locale,
            first
        )
        .execute(&mut *tx)
        .await
        .map_err(|err| err.to_string())?;
        if inserted.rows_affected() == 0 {
            return Ok(None);
        }
        let from = (!site.is_empty()).then_some(site);
        let mut last = None;
        for revision in &page.revisions {
            if !revision.model.is_empty() && revision.model != "wikitext" {
                continue;
            }
            let body = convert(&revision.text, options).markdown;
            let at = timestamp(&revision.timestamp).unwrap_or(first);
            // An edit without an account is credited to nobody: its address
            // is personal data the source wiki showed, not ours to keep.
            let author = match &revision.author {
                Author::User(name) => Some(name.as_str()),
                Author::Ip(_) | Author::Hidden => None,
            };
            let id = Uuid::new_v4();
            sqlx::query!(
                "INSERT INTO revisions
                   (id, page_id, author_id, body_md, content_hash, summary, is_minor, is_patrolled,
                    created_at, imported_author, imported_from)
                 VALUES ($1, $2, NULL, $3, $4, $5, $6, true, $7, $8, $9)",
                id,
                page_id,
                body,
                naw_markdown::content_hash(&body),
                revision.comment.as_deref().filter(|c| !c.is_empty()),
                revision.minor,
                at,
                author,
                from
            )
            .execute(&mut *tx)
            .await
            .map_err(|err| err.to_string())?;
            last = Some((id, at));
        }
        let Some((current, updated)) = last else {
            tx.rollback().await.map_err(|err| err.to_string())?;
            return Ok(Some(0));
        };
        sqlx::query!(
            "UPDATE pages SET current_revision_id = $1, updated_at = $2 WHERE id = $3",
            current,
            updated,
            page_id
        )
        .execute(&mut *tx)
        .await
        .map_err(|err| err.to_string())?;
        tx.commit().await.map_err(|err| err.to_string())?;
        Ok(Some(page.revisions.len()))
    }

    /// A redirect from `from` to `to`, unless a page or a redirect is there.
    async fn redirect(&self, from: &str, to: &str) -> Result<bool, String> {
        let (namespace, from_slug) = split(from);
        let (to_namespace, to_slug) = split(to);
        let taken = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM pages WHERE wiki_id = $1
                 AND namespace = ($2::text)::page_namespace AND slug = $3) AS "taken!""#,
            self.wiki_id,
            namespace,
            from_slug
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|err| err.to_string())?;
        if taken || from == to {
            return Ok(false);
        }
        let done = sqlx::query!(
            "INSERT INTO page_redirects (wiki_id, namespace, from_slug, to_namespace, to_slug)
             VALUES ($1, ($2::text)::page_namespace, $3, ($4::text)::page_namespace, $5)
             ON CONFLICT DO NOTHING",
            self.wiki_id,
            namespace,
            from_slug,
            to_namespace,
            to_slug
        )
        .execute(&self.pool)
        .await
        .map_err(|err| err.to_string())?;
        Ok(done.rows_affected() > 0)
    }
}

/// `template:infobox` as `("template", "infobox")`; an article is `main`.
fn split(path: &str) -> (&str, &str) {
    match path.split_once(':') {
        Some((namespace @ ("template" | "category"), slug)) => (namespace, slug),
        _ => ("main", path),
    }
}

fn timestamp(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// A template's slug, or its address when the engine's template names
/// cannot spell it (`Status/Legend`), as the converter calls it.
fn template_slug(title: &str) -> Option<String> {
    naw_markdown::transclude::template_slug(title).or_else(|| naw_wikitext::slug::page_slug(title))
}

/// The page's address here: `bao-the-whale`, `template:infobox`,
/// `category:vtubers`.
fn path_of(page: &Page) -> Option<String> {
    let bare = page.bare_title();
    match page.ns {
        MAIN => naw_wikitext::slug::page_slug(bare),
        TEMPLATE => template_slug(bare).map(|s| format!("template:{s}")),
        CATEGORY => {
            naw_markdown::categories::key(&bare.replace('_', " ")).map(|k| format!("category:{k}"))
        }
        _ => None,
    }
}

fn render(site: &str, t: &Totals, wrote: bool) -> String {
    let mut out = String::new();
    let site = if site.is_empty() { "the dump" } else { site };
    out.push_str(&format!("# Import report: {site}\n\n"));
    let read: usize = t.read.values().sum();
    out.push_str(&format!(
        "{read} pages read ({} revisions). {} converted, {} redirects. {} converted with nothing left out ({}%).\n\n",
        t.revisions,
        t.converted,
        t.redirects,
        t.clean,
        (t.clean * 100).checked_div(t.converted).unwrap_or(100)
    ));
    if wrote {
        out.push_str(&format!(
            "Written: {} pages with {} revisions, {} redirects. Already here and left alone: {}{}.\n\n",
            t.written,
            t.written_revisions,
            t.written_redirects,
            t.existing.len(),
            if t.existing.is_empty() {
                String::new()
            } else {
                format!(
                    " ({})",
                    t.existing.iter().take(20).cloned().collect::<Vec<_>>().join(", ")
                )
            }
        ));
    } else {
        out.push_str("Dry run: nothing was written. Add --write --wiki SLUG to import.\n\n");
    }
    out.push_str("Pages by namespace: ");
    out.push_str(
        &t.read
            .iter()
            .map(|(ns, n)| format!("{ns}: {n}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str(". Only 0 (articles), 10 (templates) and 14 (categories) are converted.\n\n");
    if !t.code_pages.is_empty() {
        out.push_str(&format!(
            "Code pages not converted ({}): {}\n\n",
            t.code_pages.len(),
            t.code_pages
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out.push_str("## Left out\n\n");
    if t.report.unsupported.is_empty() {
        out.push_str("Nothing.\n\n");
    } else {
        let mut missed: Vec<_> = t.report.unsupported.iter().collect();
        missed.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(a.0.cmp(b.0)));
        out.push_str("| What | Times | First seen |\n|---|---|---|\n");
        for (kind, u) in missed {
            out.push_str(&format!(
                "| {} | {} | `{}` |\n",
                kind.replace('|', "\\|"),
                u.count,
                u.example
                    .replace('`', "'")
                    .replace('|', "\\|")
                    .replace('\n', " ")
            ));
        }
        out.push('\n');
    }

    out.push_str("## Templates the pages call\n\n");
    if t.report.templates.is_empty() {
        out.push_str("None.\n\n");
    } else {
        let mut templates: Vec<_> = t.report.templates.iter().collect();
        templates.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let missing = templates
            .iter()
            .filter(|(slug, _)| !t.have_templates.contains(*slug))
            .count();
        out.push_str(&format!(
            "{} templates, {missing} of them not in the dump. Export them with the pages, or write them here.\n\n",
            templates.len()
        ));
        out.push_str("| Template | Calls | In the dump |\n|---|---|---|\n");
        for (slug, n) in templates.iter().take(60) {
            let have = if t.have_templates.contains(*slug) {
                "yes"
            } else {
                "no"
            };
            out.push_str(&format!("| {slug} | {n} | {have} |\n"));
        }
        out.push('\n');
    }

    out.push_str("## Files\n\n");
    out.push_str(&format!(
        "{} files are shown or linked. They are not in an XML dump and come over separately.\n\n",
        t.report.files.len()
    ));

    let broken: Vec<_> = t
        .report
        .links
        .iter()
        .filter(|slug| !t.have_pages.contains(*slug))
        .collect();
    out.push_str("## Links to pages not in the dump\n\n");
    out.push_str(&format!(
        "{} of {} linked pages.",
        broken.len(),
        t.report.links.len()
    ));
    if !broken.is_empty() {
        out.push_str(" For example: ");
        out.push_str(
            &broken
                .iter()
                .take(20)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    out.push_str("\n\n");

    if !t.worst.is_empty() {
        let mut worst = t.worst.clone();
        worst.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        out.push_str("## Pages that left the most out\n\n");
        for (n, title) in worst.iter().take(15) {
            out.push_str(&format!("- {title}: {n}\n"));
        }
        out.push('\n');
    }

    out.push_str("## Converted\n\n");
    let mut done: Vec<_> = t.report.converted.iter().collect();
    done.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    out.push_str(
        &done
            .iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push('\n');
    out
}
