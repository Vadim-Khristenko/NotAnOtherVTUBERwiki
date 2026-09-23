//! NotAnotherWiki Engine command line entry point.

mod grant;
mod seed;
mod user;

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    // .env before logging so NAW_LOG_* can live there; logging before config so
    // a broken config.toml is reported.
    dotenvy::dotenv().ok();
    let log = naw_core::logging::init();
    if log.trace {
        tracing::warn!(
            "NAW_LOG_TRACE is on: request headers are written to the log. \
             Credentials are redacted, but the log is still more sensitive than usual."
        );
    }

    match std::env::args().nth(1).as_deref() {
        Some("serve") => serve().await,
        Some("migrate") => migrate().await,
        Some("seed") => seed_command(std::env::args().skip(2).collect()).await,
        Some("grant") => grant_command(std::env::args().skip(2).collect()).await,
        Some("reindex") => reindex_command(std::env::args().skip(2).collect()).await,
        Some("user") => user_command(std::env::args().skip(2).collect()).await,
        _ => {
            eprintln!("usage: naw <serve|migrate|seed|grant|user|reindex>");
            ExitCode::FAILURE
        }
    }
}

fn load_config() -> Result<naw_core::config::Config, String> {
    naw_core::config::Config::load("config.toml").map_err(|err| err.to_string())
}

async fn migrate() -> ExitCode {
    dotenvy::dotenv().ok();
    let config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("config error: {err}");
            return ExitCode::FAILURE;
        }
    };
    let pool = match naw_core::db::connect(&config.database_url).await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("database error: {err}");
            return ExitCode::FAILURE;
        }
    };
    match naw_core::db::run_migrations(&pool).await {
        Ok(()) => {
            tracing::info!("migrations applied");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("migration error: {err}");
            ExitCode::FAILURE
        }
    }
}

const SEED_USAGE: &str = "usage: naw seed [--flavor NAME] --slug SLUG [--name NAME] [--domain HOST] [--locale LOCALE] [--vtuber NAME] [--community NAME] [--alias HOST]... [--seed-dir DIR]\n  env fallback: NAW_SEED_FLAVOR/SLUG/NAME/DOMAIN/LOCALE/VTUBER/COMMUNITY, NAW_SEED_ALIASES (comma separated), NAW_SEED_DIR";

fn take_value(raw: &[String], i: &mut usize) -> Option<String> {
    let next = raw.get(*i + 1)?;
    if next.starts_with("--") {
        return None;
    }
    *i += 1;
    Some(next.clone())
}

fn flag_or_env(flag: Option<String>, env_name: &str) -> Option<String> {
    flag.or_else(|| std::env::var(env_name).ok())
}

async fn seed_command(raw: Vec<String>) -> ExitCode {
    let mut flavor = None;
    let mut slug = None;
    let mut name = None;
    let mut domain = None;
    let mut locale = None;
    let mut vtuber = None;
    let mut community = None;
    let mut seed_dir_flag = None;
    let mut aliases: Vec<String> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--flavor" => flavor = take_value(&raw, &mut i),
            "--slug" => slug = take_value(&raw, &mut i),
            "--name" => name = take_value(&raw, &mut i),
            "--domain" => domain = take_value(&raw, &mut i),
            "--locale" => locale = take_value(&raw, &mut i),
            "--vtuber" => vtuber = take_value(&raw, &mut i),
            "--community" => community = take_value(&raw, &mut i),
            "--seed-dir" => seed_dir_flag = take_value(&raw, &mut i),
            "--alias" => {
                if let Some(alias) = take_value(&raw, &mut i) {
                    aliases.push(alias);
                }
            }
            "--help" | "-h" => {
                println!("{SEED_USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown seed flag: {other}\n{SEED_USAGE}");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }
    let Some(slug) = flag_or_env(slug, "NAW_SEED_SLUG").filter(|s| !s.trim().is_empty()) else {
        eprintln!("seed needs --slug (or NAW_SEED_SLUG)\n{SEED_USAGE}");
        return ExitCode::FAILURE;
    };
    let flavor = flag_or_env(flavor, "NAW_SEED_FLAVOR").unwrap_or_else(|| "default".to_string());
    let name = flag_or_env(name, "NAW_SEED_NAME").unwrap_or_else(|| slug.clone());
    let domain = flag_or_env(domain, "NAW_SEED_DOMAIN");
    let locale = flag_or_env(locale, "NAW_SEED_LOCALE").unwrap_or_else(|| "en".to_string());
    let vtuber = flag_or_env(vtuber, "NAW_SEED_VTUBER").unwrap_or_else(|| name.clone());
    let community =
        flag_or_env(community, "NAW_SEED_COMMUNITY").unwrap_or_else(|| "community".to_string());
    if let Ok(extra) = std::env::var("NAW_SEED_ALIASES") {
        aliases.extend(
            extra
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    dotenvy::dotenv().ok();
    let config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("config error: {err}");
            return ExitCode::FAILURE;
        }
    };
    let pool = match naw_core::db::connect(&config.database_url).await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("database error: {err}");
            return ExitCode::FAILURE;
        }
    };
    let seed_dir = flag_or_env(seed_dir_flag, "NAW_SEED_DIR").unwrap_or(config.seed_dir);
    let opts = seed::SeedOptions {
        flavor,
        slug,
        name,
        domain,
        locale,
        vtuber,
        community,
        aliases,
    };
    match seed::run(&pool, &seed_dir, &opts).await {
        Ok(()) => {
            tracing::info!("seed applied");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("seed error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Opens the pool, or prints why not and returns the failure code.
async fn pool_or_exit() -> Result<sqlx::PgPool, ExitCode> {
    let config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("config error: {err}");
            return Err(ExitCode::FAILURE);
        }
    };
    match naw_core::db::connect(&config.database_url).await {
        Ok(pool) => Ok(pool),
        Err(err) => {
            eprintln!("database error: {err}");
            Err(ExitCode::FAILURE)
        }
    }
}

async fn grant_command(raw: Vec<String>) -> ExitCode {
    let args = match grant::parse(&raw) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{err}\n\n{}", grant::USAGE);
            return ExitCode::FAILURE;
        }
    };
    let pool = match pool_or_exit().await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    match grant::run(&pool, &args).await {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("grant error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn user_command(raw: Vec<String>) -> ExitCode {
    let command = match user::parse(&raw) {
        Ok(command) => command,
        Err(err) => {
            eprintln!(
                "{err}

{}",
                user::USAGE
            );
            return ExitCode::FAILURE;
        }
    };
    let pool = match pool_or_exit().await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    match user::run(&pool, command).await {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("user error: {err}");
            ExitCode::FAILURE
        }
    }
}

const REINDEX_USAGE: &str =
    "usage: naw reindex [--wiki SLUG]\n  with no --wiki, rebuilds the search index for every wiki";

/// Rebuilds the search index.
async fn reindex_command(raw: Vec<String>) -> ExitCode {
    let mut slug: Option<String> = None;
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--wiki" => match raw.get(i + 1) {
                Some(value) if !value.starts_with("--") => {
                    slug = Some(value.clone());
                    i += 2;
                }
                _ => {
                    eprintln!("--wiki needs a value\n\n{REINDEX_USAGE}");
                    return ExitCode::FAILURE;
                }
            },
            other => {
                eprintln!("unknown flag {other}\n\n{REINDEX_USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    let pool = match pool_or_exit().await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let wiki_id = match &slug {
        Some(slug) => {
            match sqlx::query!("SELECT id FROM wikis WHERE slug = $1", slug)
                .fetch_optional(&pool)
                .await
            {
                Ok(Some(row)) => Some(row.id),
                Ok(None) => {
                    eprintln!("no wiki with slug {slug}");
                    return ExitCode::FAILURE;
                }
                Err(err) => {
                    eprintln!("database error: {err}");
                    return ExitCode::FAILURE;
                }
            }
        }
        None => None,
    };
    match naw_core::search::reindex(&pool, wiki_id).await {
        Ok(count) => {
            println!("reindexed {count} page(s)");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("reindex error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> ExitCode {
    dotenvy::dotenv().ok();
    let config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("config error: {err}");
            return ExitCode::FAILURE;
        }
    };
    let state = match naw_core::state::build(config).await {
        Ok(state) => state,
        Err(err) => {
            eprintln!("state error: {err}");
            return ExitCode::FAILURE;
        }
    };
    // A wiki that cannot guarantee its owner does not start serving.
    match naw_web::bootstrap::ensure_owner(&state).await {
        Ok(Some((username, outcome))) => {
            tracing::info!(%username, ?outcome, "bootstrap owner ensured");
            if std::env::var("NAW_BOOTSTRAP_OWNER_PASSWORD").is_ok() {
                tracing::warn!(
                    "the bootstrap owner password is in the environment. Prefer NAW_BOOTSTRAP_OWNER_PASSWORD_FILE, and change the password after the first sign-in: a changed password is never overwritten"
                );
            }
        }
        Ok(None) => {}
        Err(err) => {
            eprintln!("bootstrap owner error: {err}");
            return ExitCode::FAILURE;
        }
    }
    let addr = format!("{}:{}", state.config.http_bind, state.config.http_port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("bind error on {addr}: {err}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(skin_dir = %state.config.skin_dir, %addr, "naw listening");
    match axum::serve(
        listener,
        naw_web::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("server error: {err}");
            ExitCode::FAILURE
        }
    }
}
