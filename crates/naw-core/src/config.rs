//! Configuration loading from config.toml plus environment overrides.

use std::fmt;
use std::path::Path;

use serde::Deserialize;

use crate::error::AppError;

#[derive(Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_database_url")]
    pub database_url: String,
    #[serde(default = "default_valkey_url")]
    pub valkey_url: String,
    #[serde(default = "default_http_bind")]
    pub http_bind: String,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_storage_root")]
    pub storage_root: String,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Credentials live inside the URLs. Never print them.
        f.debug_struct("Config")
            .field("database_url", &"[redacted]")
            .field("valkey_url", &"[redacted]")
            .field("http_bind", &self.http_bind)
            .field("http_port", &self.http_port)
            .field("storage_root", &self.storage_root)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_url: default_database_url(),
            valkey_url: default_valkey_url(),
            http_bind: default_http_bind(),
            http_port: default_http_port(),
            storage_root: default_storage_root(),
        }
    }
}

fn default_database_url() -> String {
    "postgres://naw:naw@localhost:5433/naw_dev".to_string()
}

fn default_valkey_url() -> String {
    "redis://localhost:6380".to_string()
}

fn default_http_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_http_port() -> u16 {
    8080
}

fn default_storage_root() -> String {
    "./uploads".to_string()
}

impl Config {
    /// Loads from a TOML file, then applies env overrides. A missing file is
    /// not an error: defaults target the local compose stack.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let mut cfg: Self = match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str(&raw).map_err(|err| AppError::Config(err.to_string()))?,
            Err(_) => Self::default(),
        };
        if let Ok(value) =
            std::env::var("NAW_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL"))
        {
            cfg.database_url = value;
        }
        if let Ok(value) = std::env::var("NAW_VALKEY_URL").or_else(|_| std::env::var("VALKEY_URL"))
        {
            cfg.valkey_url = value;
        }
        if let Ok(value) = std::env::var("NAW_HTTP_BIND") {
            cfg.http_bind = value;
        }
        if let Ok(value) = std::env::var("NAW_HTTP_PORT") {
            cfg.http_port = value
                .parse()
                .map_err(|err| AppError::Config(format!("NAW_HTTP_PORT: {err}")))?;
        }
        if let Ok(value) = std::env::var("NAW_STORAGE_ROOT") {
            cfg.storage_root = value;
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_leaks_urls() {
        let cfg = Config::default();
        let dumped = format!("{cfg:?}");
        assert!(!dumped.contains("postgres://"));
        assert!(!dumped.contains("redis://"));
        assert!(dumped.contains("[redacted]"));
    }
}
