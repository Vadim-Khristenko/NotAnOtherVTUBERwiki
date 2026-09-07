//! Pluggable backend traits plus scaffold stubs.
//!
//! `dyn` is deliberate here: the active backend is chosen at runtime from
//! configuration, not at compile time.

use async_trait::async_trait;
use std::fmt;

use crate::error::AppError;

#[async_trait]
pub trait SearchBackend: fmt::Debug + Send + Sync {
    async fn ping(&self) -> Result<(), AppError>;
}

#[derive(Debug, Default)]
pub struct NoopSearch;

#[async_trait]
impl SearchBackend for NoopSearch {
    async fn ping(&self) -> Result<(), AppError> {
        Ok(())
    }
}

#[async_trait]
pub trait StorageBackend: fmt::Debug + Send + Sync {
    async fn ping(&self) -> Result<(), AppError>;
}

#[derive(Debug)]
pub struct LocalStorage {
    // Scaffold stub: real object operations land after Phase 1, so the
    // handle is currently write-only. Allowed until then.
    #[allow(dead_code)]
    store: object_store::local::LocalFileSystem,
}

impl LocalStorage {
    pub fn new(root: &str) -> Result<Self, AppError> {
        // The server owns its data directory: create it on boot so a fresh
        // checkout serves without manual setup. Only the path is reported.
        std::fs::create_dir_all(root)
            .map_err(|err| AppError::Config(format!("storage root {root}: {err}")))?;
        let store = object_store::local::LocalFileSystem::new_with_prefix(root)
            .map_err(|err| AppError::Config(err.to_string()))?;
        Ok(Self { store })
    }
}

#[async_trait]
impl StorageBackend for LocalStorage {
    async fn ping(&self) -> Result<(), AppError> {
        Ok(())
    }
}
