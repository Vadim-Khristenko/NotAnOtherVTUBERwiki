//! NotAnotherWiki Engine command line entry point.

mod seed;

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "naw=info,tower_http=info".into()),
        )
        .init();

    match std::env::args().nth(1).as_deref() {
        Some("serve") => serve().await,
        Some("migrate") => migrate().await,
        Some("seed") => seed_command(std::env::args().skip(2).collect()).await,
        _ => {
            eprintln!("usage: naw <serve|migrate|seed>");
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
    match seed::run(&pool, &config.skin_dir, &seed_dir, &opts).await {
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
    let addr = format!("{}:{}", state.config.http_bind, state.config.http_port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("bind error on {addr}: {err}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(%addr, "naw listening");
    match axum::serve(listener, naw_web::router(state)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("server error: {err}");
            ExitCode::FAILURE
        }
    }
}
