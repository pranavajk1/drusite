// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attachments in any S3-compatible store, sealed with the workspace's key.
//!
//! Objects are content-addressed, so a file used on many pages is stored once. The address is a
//! BLAKE3 hash keyed by the workspace's key, not a plain hash of the content: someone with
//! access to the bucket alone can't confirm that a workspace holds a file they already have.

use std::sync::Arc;

use bytes::Bytes;
use drusite_model::WorkspaceId;
use object_store::aws::AmazonS3Builder;
use object_store::memory::InMemory;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};

use crate::config::StorageConfig;
use crate::crypto::{CryptoError, WorkspaceKey, sealed_key_version};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("object store: {0}")]
    Store(#[from] object_store::Error),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("blob {0} isn't there")]
    NotFound(String),
    #[error("blob {0} doesn't match its address")]
    Mismatch(String),
}

/// A blob's address within its workspace: 64 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobId(String);

impl BlobId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Accepts only 64 lowercase hex digits, so an ID can't climb out of its workspace's prefix.
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| Self(s.to_owned()))
    }
}

#[derive(Debug, Clone)]
pub struct BlobStore {
    store: Arc<dyn ObjectStore>,
    prefix: Path,
}

impl BlobStore {
    pub fn from_config(config: &StorageConfig) -> Result<Self, StorageError> {
        let mut builder = AmazonS3Builder::from_env()
            .with_bucket_name(&config.bucket)
            .with_region(&config.region)
            .with_virtual_hosted_style_request(config.virtual_hosted_style);
        if let Some(endpoint) = &config.endpoint {
            builder = builder
                .with_endpoint(endpoint.as_str().trim_end_matches('/'))
                .with_allow_http(endpoint.scheme() == "http");
        }
        if let (Some(id), Some(secret)) = (&config.access_key_id, &config.secret_access_key) {
            builder = builder
                .with_access_key_id(id.expose())
                .with_secret_access_key(secret.expose());
        }
        Ok(Self {
            store: Arc::new(builder.build()?),
            prefix: Path::from(config.prefix.trim_matches('/')),
        })
    }

    /// A store in memory, for tests.
    pub fn in_memory() -> Self {
        Self {
            store: Arc::new(InMemory::new()),
            prefix: Path::default(),
        }
    }

    fn path(&self, parts: &[&str]) -> Path {
        let mut path = self.prefix.clone();
        for part in parts {
            path = path.join(*part);
        }
        path
    }

    fn blob_path(&self, ws: WorkspaceId, id: &BlobId) -> Path {
        self.path(&["workspaces", &ws.to_string(), "blobs", id.as_str()])
    }

    /// The blob's address: its content hashed with a key derived from the workspace key.
    pub fn address(key: &WorkspaceKey, content: &[u8]) -> BlobId {
        let naming = key.derive("drusite 2026 blob address");
        BlobId(blake3::keyed_hash(&naming, content).to_hex().to_string())
    }

    /// Stores `content` sealed with `key` and returns its address. Storing the same content
    /// again writes the same address.
    pub async fn put(&self, key: &WorkspaceKey, content: &[u8]) -> Result<BlobId, StorageError> {
        let id = Self::address(key, content);
        let sealed = key.encrypt(id.as_str().as_bytes(), content)?;
        self.store
            .put(
                &self.blob_path(key.workspace(), &id),
                PutPayload::from(sealed),
            )
            .await?;
        Ok(id)
    }

    /// The key version a stored blob was sealed with, to fetch that key before [`Self::get`].
    pub async fn key_version(&self, ws: WorkspaceId, id: &BlobId) -> Result<u32, StorageError> {
        let head = self
            .store
            .get_range(&self.blob_path(ws, id), 0..5)
            .await
            .map_err(|e| not_found(e, id))?;
        Ok(sealed_key_version(&head)?)
    }

    pub async fn get(&self, key: &WorkspaceKey, id: &BlobId) -> Result<Bytes, StorageError> {
        let sealed = self
            .store
            .get(&self.blob_path(key.workspace(), id))
            .await
            .map_err(|e| not_found(e, id))?
            .bytes()
            .await?;
        let content = key.decrypt(id.as_str().as_bytes(), &sealed)?;
        if &Self::address(key, &content) != id {
            return Err(StorageError::Mismatch(id.0.clone()));
        }
        Ok(Bytes::from(content))
    }

    pub async fn delete(&self, ws: WorkspaceId, id: &BlobId) -> Result<(), StorageError> {
        self.store.delete(&self.blob_path(ws, id)).await?;
        Ok(())
    }

    /// Proves the store is reachable and writable: writes, reads back and deletes a small
    /// object under `_drusite/`.
    pub async fn check_writable(&self) -> Result<(), StorageError> {
        let path = self.path(&["_drusite", &format!("probe-{}", uuid::Uuid::now_v7())]);
        let body = Bytes::from_static(b"drusite storage probe");
        self.store
            .put(&path, PutPayload::from(body.clone()))
            .await?;
        let back = self.store.get(&path).await?.bytes().await?;
        self.store.delete(&path).await?;
        if back != body {
            return Err(StorageError::Mismatch(path.to_string()));
        }
        Ok(())
    }

    /// A cheap reachability check for readiness: reads an object that needn't exist.
    pub async fn check_reachable(&self) -> Result<(), StorageError> {
        match self.store.head(&self.path(&["_drusite", "ready"])).await {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

fn not_found(e: object_store::Error, id: &BlobId) -> StorageError {
    match e {
        object_store::Error::NotFound { .. } => StorageError::NotFound(id.0.clone()),
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blobs_round_trip_sealed_and_deduplicated() {
        let store = BlobStore::in_memory();
        let ws = WorkspaceId::new();
        let key = WorkspaceKey::generate(ws, 1).unwrap();
        let id = store.put(&key, b"a picture").await.unwrap();
        assert_eq!(store.put(&key, b"a picture").await.unwrap(), id);
        assert_eq!(store.key_version(ws, &id).await.unwrap(), 1);
        assert_eq!(&store.get(&key, &id).await.unwrap()[..], b"a picture");

        // What's stored is ciphertext.
        let raw = store
            .store
            .get(&store.blob_path(ws, &id))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert!(!raw.windows(9).any(|w| w == b"a picture"));

        // The same file in another workspace gets another address.
        let other = WorkspaceKey::generate(WorkspaceId::new(), 1).unwrap();
        assert_ne!(BlobStore::address(&other, b"a picture"), id);

        store.delete(ws, &id).await.unwrap();
        assert!(matches!(
            store.get(&key, &id).await,
            Err(StorageError::NotFound(_))
        ));
        store.check_writable().await.unwrap();
        store.check_reachable().await.unwrap();
    }

    #[test]
    fn blob_ids_are_only_hex() {
        assert!(BlobId::parse(&"a".repeat(64)).is_some());
        assert!(BlobId::parse("../../etc").is_none());
        assert!(BlobId::parse(&"A".repeat(64)).is_none());
    }
}
