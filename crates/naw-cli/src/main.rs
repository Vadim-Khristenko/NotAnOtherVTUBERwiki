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
        Some("seed") => seed().await,
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

async fn seed() -> ExitCode {
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
    match seed::run(&pool, &config.skin_dir).await {
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
