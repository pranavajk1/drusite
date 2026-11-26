// SPDX-License-Identifier: AGPL-3.0-or-later

//! The key source, and workspace keys stored in Postgres wrapped by it.
//!
//! A key source wraps and unwraps workspace keys. The `env` and `file` sources hold master keys
//! in Drusite's memory; the `transit` source asks a KMS (the Transit engine of OpenBao or
//! HashiCorp Vault) to do it, so the master key never leaves the KMS. Anything that can wrap and
//! unwrap 32 bytes can be added as another variant.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use drusite_model::WorkspaceId;
use serde::Deserialize;
use sqlx::PgConnection;
use url::Url;
use zeroize::Zeroizing;

use crate::config::{KeysConfig, Secret};
use crate::crypto::{CryptoError, KEY_LEN, LocalMasterKeys, WorkspaceKey, wrap_aad};

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("master keys: {0}")]
    Source(String),
    #[error("the KMS: {0}")]
    Kms(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error("workspace {0} has no key")]
    NoKey(WorkspaceId),
}

#[derive(Debug)]
pub enum KeySource {
    Local(LocalMasterKeys),
    Transit(Box<Transit>),
}

impl KeySource {
    pub fn from_config(config: &KeysConfig) -> Result<Self, KeyError> {
        match config {
            KeysConfig::Env { var } => {
                let text = Zeroizing::new(std::env::var(var).map_err(|_| {
                    KeyError::Source(format!(
                        "[keys] source is env, but {var} isn't set; make a key with \
                         `drusite-server keys generate`"
                    ))
                })?);
                Ok(Self::Local(LocalMasterKeys::parse(&text)?))
            }
            KeysConfig::File { path } => {
                let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| {
                    KeyError::Source(format!("can't read {}: {e}", path.display()))
                })?);
                Ok(Self::Local(LocalMasterKeys::parse(&text)?))
            }
            KeysConfig::Transit {
                address,
                key,
                mount,
                token,
                namespace,
            } => Ok(Self::Transit(Box::new(Transit::new(
                address.clone(),
                mount,
                key,
                token.clone(),
                namespace.clone(),
            )?))),
        }
    }

    /// Wraps a workspace key; returns the master key's ID and the wrapped bytes.
    pub async fn wrap(&self, key: &WorkspaceKey) -> Result<(String, Vec<u8>), KeyError> {
        match self {
            Self::Local(keys) => Ok(keys.wrap(key)?),
            Self::Transit(transit) => transit.wrap(key).await,
        }
    }

    pub async fn unwrap(
        &self,
        workspace: WorkspaceId,
        version: u32,
        wrapped: &[u8],
    ) -> Result<WorkspaceKey, KeyError> {
        match self {
            Self::Local(keys) => Ok(keys.unwrap(workspace, version, wrapped)?),
            Self::Transit(transit) => transit.unwrap(workspace, version, wrapped).await,
        }
    }

    /// Proves the source works by wrapping and unwrapping a throwaway key.
    pub async fn check(&self) -> Result<(), KeyError> {
        let ws = WorkspaceId::new();
        let key = WorkspaceKey::generate(ws, 1)?;
        let (_, wrapped) = self.wrap(&key).await?;
        let back = self.unwrap(ws, 1, &wrapped).await?;
        if back.bytes() != key.bytes() {
            return Err(KeyError::Source(
                "a wrapped key came back different".to_owned(),
            ));
        }
        Ok(())
    }
}

/// The Transit secrets engine of OpenBao or HashiCorp Vault.
#[derive(Debug)]
pub struct Transit {
    client: reqwest::Client,
    encrypt_url: Url,
    decrypt_url: Url,
    key: String,
    token: Secret,
    namespace: Option<String>,
}

#[derive(Deserialize)]
struct TransitResponse {
    data: TransitData,
}

#[derive(Deserialize)]
struct TransitData {
    #[serde(default)]
    ciphertext: Option<String>,
    #[serde(default)]
    plaintext: Option<String>,
}

impl Transit {
    fn new(
        address: Url,
        mount: &str,
        key: &str,
        token: Secret,
        namespace: Option<String>,
    ) -> Result<Self, KeyError> {
        let endpoint = |op: &str| {
            let mut url = address.clone();
            url.path_segments_mut()
                .map_err(|()| KeyError::Kms(format!("{address} can't be a base URL")))?
                .pop_if_empty()
                .extend(["v1"])
                .extend(mount.split('/').filter(|s| !s.is_empty()))
                .extend([op, key]);
            Ok::<_, KeyError>(url)
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| KeyError::Kms(e.to_string()))?;
        Ok(Self {
            client,
            encrypt_url: endpoint("encrypt")?,
            decrypt_url: endpoint("decrypt")?,
            key: key.to_owned(),
            token,
            namespace,
        })
    }

    async fn call(&self, url: &Url, body: serde_json::Value) -> Result<TransitData, KeyError> {
        let mut request = self
            .client
            .post(url.clone())
            .header("X-Vault-Token", self.token.expose())
            .json(&body);
        if let Some(ns) = &self.namespace {
            request = request.header("X-Vault-Namespace", ns);
        }
        let response = request
            .send()
            .await
            .map_err(|e| KeyError::Kms(format!("{}: {e}", url.path())))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(KeyError::Kms(format!(
                "{} returned {status}: {text}",
                url.path()
            )));
        }
        let parsed: TransitResponse = response
            .json()
            .await
            .map_err(|e| KeyError::Kms(format!("{}: {e}", url.path())))?;
        Ok(parsed.data)
    }

    async fn wrap(&self, key: &WorkspaceKey) -> Result<(String, Vec<u8>), KeyError> {
        let plaintext = Zeroizing::new(STANDARD.encode(key.bytes()));
        let data = self
            .call(
                &self.encrypt_url,
                serde_json::json!({
                    "plaintext": plaintext.as_str(),
                    "associated_data": STANDARD.encode(wrap_aad(key.workspace(), key.version())),
                }),
            )
            .await?;
        let ciphertext = data
            .ciphertext
            .ok_or_else(|| KeyError::Kms("encrypt returned no ciphertext".to_owned()))?;
        // `vault:v3:...`: the Transit key's version is part of the ID, so rotation shows.
        let version = ciphertext.split(':').nth(1).unwrap_or("v?");
        Ok((
            format!("transit:{}:{version}", self.key),
            ciphertext.into_bytes(),
        ))
    }

    async fn unwrap(
        &self,
        workspace: WorkspaceId,
        version: u32,
        wrapped: &[u8],
    ) -> Result<WorkspaceKey, KeyError> {
        let ciphertext = std::str::from_utf8(wrapped).map_err(|_| CryptoError::Format)?;
        let data = self
            .call(
                &self.decrypt_url,
                serde_json::json!({
                    "ciphertext": ciphertext,
                    "associated_data": STANDARD.encode(wrap_aad(workspace, version)),
                }),
            )
            .await?;
        let plaintext = Zeroizing::new(
            data.plaintext
                .ok_or_else(|| KeyError::Kms("decrypt returned no plaintext".to_owned()))?,
        );
        let raw = Zeroizing::new(
            STANDARD
                .decode(plaintext.as_bytes())
                .map_err(|_| CryptoError::Format)?,
        );
        let key: [u8; KEY_LEN] = raw.as_slice().try_into().map_err(|_| CryptoError::Format)?;
        Ok(WorkspaceKey::from_bytes(
            workspace,
            version,
            Zeroizing::new(key),
        ))
    }
}

/// Workspace keys: made when a workspace is, stored wrapped in `workspace_keys`, unwrapped on
/// first use and kept in memory after that.
#[derive(Debug)]
pub struct Keyring {
    source: KeySource,
    cache: RwLock<HashMap<(WorkspaceId, u32), Arc<WorkspaceKey>>>,
}

impl Keyring {
    pub fn new(source: KeySource) -> Self {
        Self {
            source,
            cache: RwLock::default(),
        }
    }

    pub const fn source(&self) -> &KeySource {
        &self.source
    }

    fn cached(&self, ws: WorkspaceId, version: u32) -> Option<Arc<WorkspaceKey>> {
        self.cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(ws, version))
            .cloned()
    }

    fn remember(&self, key: WorkspaceKey) -> Arc<WorkspaceKey> {
        let key = Arc::new(key);
        self.cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((key.workspace(), key.version()), Arc::clone(&key));
        key
    }

    /// Makes the next version of a workspace's key and stores it wrapped. The transaction must
    /// be allowed to (the workspace's owner, or the system).
    pub async fn create(
        &self,
        conn: &mut PgConnection,
        ws: WorkspaceId,
    ) -> Result<Arc<WorkspaceKey>, KeyError> {
        let latest: Option<i32> =
            sqlx::query_scalar("SELECT max(version) FROM workspace_keys WHERE workspace_id = $1")
                .bind(ws.as_uuid())
                .fetch_one(&mut *conn)
                .await?;
        let version = latest.map_or(1, |v| v + 1);
        let version_u32 = u32::try_from(version).map_err(|_| CryptoError::Format)?;
        let key = WorkspaceKey::generate(ws, version_u32)?;
        let (master_key_id, wrapped) = self.source.wrap(&key).await?;
        sqlx::query(
            "INSERT INTO workspace_keys (workspace_id, version, master_key_id, wrapped)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(ws.as_uuid())
        .bind(version)
        .bind(master_key_id)
        .bind(wrapped)
        .execute(&mut *conn)
        .await?;
        Ok(self.remember(key))
    }

    /// The newest version of a workspace's key, for sealing.
    pub async fn current(
        &self,
        conn: &mut PgConnection,
        ws: WorkspaceId,
    ) -> Result<Arc<WorkspaceKey>, KeyError> {
        let version: Option<i32> =
            sqlx::query_scalar("SELECT max(version) FROM workspace_keys WHERE workspace_id = $1")
                .bind(ws.as_uuid())
                .fetch_one(&mut *conn)
                .await?;
        let version = version.ok_or(KeyError::NoKey(ws))?;
        self.version(
            conn,
            ws,
            u32::try_from(version).map_err(|_| CryptoError::Format)?,
        )
        .await
    }

    /// One version of a workspace's key, for opening what it sealed.
    pub async fn version(
        &self,
        conn: &mut PgConnection,
        ws: WorkspaceId,
        version: u32,
    ) -> Result<Arc<WorkspaceKey>, KeyError> {
        // Read through the transaction even when cached, so row-level security still decides
        // whether this caller may have the key.
        let wrapped: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT wrapped FROM workspace_keys WHERE workspace_id = $1 AND version = $2",
        )
        .bind(ws.as_uuid())
        .bind(i32::try_from(version).map_err(|_| CryptoError::Format)?)
        .fetch_optional(&mut *conn)
        .await?;
        let wrapped = wrapped.ok_or(KeyError::NoKey(ws))?;
        if let Some(key) = self.cached(ws, version) {
            return Ok(key);
        }
        let key = self.source.unwrap(ws, version, &wrapped).await?;
        Ok(self.remember(key))
    }
}
