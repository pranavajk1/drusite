// SPDX-License-Identifier: AGPL-3.0-or-later

//! The S3 client and the KMS key source against real services. Each test is skipped unless its
//! variables are set (CI sets them):
//!
//! - `DRUSITE_TEST_S3_ENDPOINT`, `_BUCKET`, `_ACCESS_KEY`, `_SECRET_KEY`: any S3-compatible store
//!   with an existing bucket.
//! - `DRUSITE_TEST_TRANSIT_ADDR`, `_TOKEN`: OpenBao or Vault with Transit at `transit/` and a
//!   key named `drusite`.

#![allow(clippy::unwrap_used)] // helpers outside #[test] functions

use drusite_model::WorkspaceId;
use drusite_server::config::{KeysConfig, Secret, StorageConfig};
use drusite_server::crypto::WorkspaceKey;
use drusite_server::keys::KeySource;
use drusite_server::storage::{BlobStore, StorageError};

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn s3_config(prefix: &str, secret: Option<&str>) -> Option<StorageConfig> {
    Some(StorageConfig {
        bucket: var("DRUSITE_TEST_S3_BUCKET")?,
        endpoint: Some(var("DRUSITE_TEST_S3_ENDPOINT")?.parse().unwrap()),
        region: "us-east-1".to_owned(),
        access_key_id: Some(Secret::new(var("DRUSITE_TEST_S3_ACCESS_KEY")?)),
        secret_access_key: Some(Secret::new(
            secret.map_or_else(|| var("DRUSITE_TEST_S3_SECRET_KEY"), |s| Some(s.to_owned()))?,
        )),
        virtual_hosted_style: false,
        prefix: prefix.to_owned(),
    })
}

#[tokio::test]
async fn s3_compatible_store() {
    let prefix = format!("test-{}", uuid::Uuid::now_v7());
    let Some(config) = s3_config(&prefix, None) else {
        eprintln!("skipped: set DRUSITE_TEST_S3_* to test against an S3-compatible store");
        return;
    };
    let store = BlobStore::from_config(&config).unwrap();
    store.check_writable().await.unwrap();
    store.check_reachable().await.unwrap();

    let ws = WorkspaceId::new();
    let key = WorkspaceKey::generate(ws, 1).unwrap();
    let content: Vec<u8> = (0..200_000u32).flat_map(u32::to_le_bytes).collect();
    let id = store.put(&key, &content).await.unwrap();
    assert_eq!(store.key_version(ws, &id).await.unwrap(), 1);
    assert_eq!(
        store.get(&key, &id).await.unwrap().as_ref(),
        content.as_slice()
    );
    store.delete(ws, &id).await.unwrap();
    assert!(matches!(
        store.get(&key, &id).await,
        Err(StorageError::NotFound(_))
    ));

    // Wrong credentials fail the start-up check rather than the first upload.
    let bad = BlobStore::from_config(&s3_config(&prefix, Some("wrong-secret")).unwrap()).unwrap();
    assert!(bad.check_writable().await.is_err());
}

#[tokio::test]
async fn transit_key_source() {
    let (Some(address), Some(token)) = (
        var("DRUSITE_TEST_TRANSIT_ADDR"),
        var("DRUSITE_TEST_TRANSIT_TOKEN"),
    ) else {
        eprintln!("skipped: set DRUSITE_TEST_TRANSIT_* to test the Transit key source");
        return;
    };
    let config = |token: &str| KeysConfig::Transit {
        address: address.parse().unwrap(),
        key: "drusite".to_owned(),
        mount: "transit".to_owned(),
        token: Secret::new(token),
        namespace: None,
    };
    let source = KeySource::from_config(&config(&token)).unwrap();
    source.check().await.unwrap();

    let ws = WorkspaceId::new();
    let key = WorkspaceKey::generate(ws, 1).unwrap();
    let (id, wrapped) = source.wrap(&key).await.unwrap();
    assert!(id.starts_with("transit:drusite:v"), "{id}");
    let back = source.unwrap(ws, 1, &wrapped).await.unwrap();
    let sealed = key.encrypt(b"c", b"m").unwrap();
    assert_eq!(back.decrypt(b"c", &sealed).unwrap(), b"m");

    // The wrapped key is bound to its workspace and version by the KMS too.
    assert!(
        source
            .unwrap(WorkspaceId::new(), 1, &wrapped)
            .await
            .is_err()
    );
    assert!(source.unwrap(ws, 2, &wrapped).await.is_err());
    // And a bad token is refused.
    let refused = KeySource::from_config(&config("not-a-token")).unwrap();
    assert!(refused.check().await.is_err());
}
