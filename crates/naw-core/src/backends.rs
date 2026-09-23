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
    /// Stores `data` under `key`, replacing whatever was there. Keys are
    /// slash separated, like `media/ab/abcd....png`.
    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), AppError>;
    /// The object at `key`, or `None` when there is none.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, AppError>;
    /// Whether `key` holds an object, without reading it.
    async fn exists(&self, key: &str) -> Result<bool, AppError>;
    /// Removes `key`. Removing something that is not there is not an error.
    async fn delete(&self, key: &str) -> Result<(), AppError>;
}

#[derive(Debug)]
pub struct LocalStorage {
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

/// A storage key as an object_store path. Keys come from the server (hashes
/// and fixed prefixes), never from a request, but a malformed one still fails
/// here rather than escaping the root.
fn object_path(key: &str) -> Result<object_store::path::Path, AppError> {
    object_store::path::Path::parse(key).map_err(|err| {
        tracing::error!(error = %err, "invalid storage key");
        AppError::Internal
    })
}

fn storage_error(err: object_store::Error) -> AppError {
    tracing::error!(error = %err, "storage operation failed");
    AppError::Internal
}

#[async_trait]
impl StorageBackend for LocalStorage {
    async fn ping(&self) -> Result<(), AppError> {
        Ok(())
    }

    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), AppError> {
        use object_store::ObjectStoreExt;
        self.store
            .put(&object_path(key)?, object_store::PutPayload::from(data))
            .await
            .map(|_| ())
            .map_err(storage_error)
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, AppError> {
        use object_store::ObjectStoreExt;
        match self.store.get(&object_path(key)?).await {
            Ok(result) => Ok(Some(result.bytes().await.map_err(storage_error)?.to_vec())),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(err) => Err(storage_error(err)),
        }
    }

    async fn exists(&self, key: &str) -> Result<bool, AppError> {
        use object_store::ObjectStoreExt;
        match self.store.head(&object_path(key)?).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(err) => Err(storage_error(err)),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), AppError> {
        use object_store::ObjectStoreExt;
        match self.store.delete(&object_path(key)?).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(err) => Err(storage_error(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_storage_round_trips() {
        let root = std::env::temp_dir().join(format!("naw-storage-{}", std::process::id()));
        let storage = LocalStorage::new(root.to_str().unwrap()).expect("root");
        assert_eq!(storage.get("media/ab/x.png").await.unwrap(), None);
        assert!(!storage.exists("media/ab/x.png").await.unwrap());
        storage
            .put("media/ab/x.png", b"bytes".to_vec())
            .await
            .unwrap();
        assert_eq!(
            storage.get("media/ab/x.png").await.unwrap(),
            Some(b"bytes".to_vec())
        );
        assert!(storage.exists("media/ab/x.png").await.unwrap());
        storage.delete("media/ab/x.png").await.unwrap();
        storage.delete("media/ab/x.png").await.unwrap();
        assert_eq!(storage.get("media/ab/x.png").await.unwrap(), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
