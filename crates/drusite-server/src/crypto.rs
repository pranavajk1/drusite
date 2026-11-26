// SPDX-License-Identifier: AGPL-3.0-or-later

//! Envelope encryption at rest, with no I/O.
//!
//! Each workspace has its own data key (32 random bytes). Everything the server stores for a
//! workspace (document updates, snapshots, attachments) is sealed with it using
//! XChaCha20-Poly1305: a random 192-bit nonce per message, so nonces never need tracking. The
//! data key is stored only wrapped by a master key from the key source (see [`crate::keys`]),
//! so a database backup or an S3 bucket on its own is ciphertext.
//!
//! Associated data binds every ciphertext to its workspace, key version and a caller-chosen
//! context (an object's name, a document and sequence number), so a ciphertext copied to
//! another row or object fails to open rather than being read as that row's data.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use drusite_model::WorkspaceId;
use zeroize::Zeroizing;

pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

/// The first byte of a sealed message: the format, so it can change later.
const DATA_FORMAT_V1: u8 = 1;
/// The first bytes of a data key wrapped by a local master key.
const WRAP_MAGIC_V1: &[u8; 4] = b"DMK1";

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("ciphertext is damaged, or belongs to another workspace, key or object")]
    Open,
    #[error("ciphertext is in an unknown format")]
    Format,
    #[error("sealed with workspace key version {found}, not {expected}")]
    WrongKeyVersion { expected: u32, found: u32 },
    #[error("wrapped by master key {0:?}, which the key source doesn't have")]
    UnknownMasterKey(String),
    #[error("master keys: {0}")]
    MasterKeys(String),
    #[error("the system's random number generator failed")]
    Random,
}

pub(crate) fn random_bytes<const N: usize>() -> Result<Zeroizing<[u8; N]>, CryptoError> {
    let mut buf = Zeroizing::new([0u8; N]);
    getrandom::fill(buf.as_mut()).map_err(|_| CryptoError::Random)?;
    Ok(buf)
}

fn seal(
    key: &[u8; KEY_LEN],
    aad: &[u8],
    plaintext: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), CryptoError> {
    let nonce = random_bytes::<NONCE_LEN>()?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let ct = cipher
        .encrypt(
            &XNonce::from(*nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Open)?;
    out.extend_from_slice(nonce.as_ref());
    out.extend_from_slice(&ct);
    Ok(())
}

fn open(key: &[u8; KEY_LEN], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(CryptoError::Format);
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            &XNonce::try_from(nonce).map_err(|_| CryptoError::Format)?,
            Payload { msg: ct, aad },
        )
        .map_err(|_| CryptoError::Open)
}

/// A workspace's data key, one version of it.
pub struct WorkspaceKey {
    workspace: WorkspaceId,
    version: u32,
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl fmt::Debug for WorkspaceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceKey")
            .field("workspace", &self.workspace)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl WorkspaceKey {
    /// A new random key.
    pub fn generate(workspace: WorkspaceId, version: u32) -> Result<Self, CryptoError> {
        Ok(Self {
            workspace,
            version,
            key: random_bytes()?,
        })
    }

    pub fn from_bytes(workspace: WorkspaceId, version: u32, key: Zeroizing<[u8; KEY_LEN]>) -> Self {
        Self {
            workspace,
            version,
            key,
        }
    }

    pub const fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub(crate) fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.key
    }

    fn aad(&self, context: &[u8]) -> Vec<u8> {
        let mut aad = Vec::with_capacity(48 + context.len());
        aad.extend_from_slice(b"drusite data v1\0");
        aad.extend_from_slice(self.workspace.as_uuid().as_bytes());
        aad.extend_from_slice(&self.version.to_be_bytes());
        aad.extend_from_slice(context);
        aad
    }

    /// Seals `plaintext`. `context` names what it is (an object key, a document and sequence
    /// number); the same context is needed to open it.
    pub fn encrypt(&self, context: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = Vec::with_capacity(1 + 4 + NONCE_LEN + plaintext.len() + TAG_LEN);
        out.push(DATA_FORMAT_V1);
        out.extend_from_slice(&self.version.to_be_bytes());
        seal(&self.key, &self.aad(context), plaintext, &mut out)?;
        Ok(out)
    }

    pub fn decrypt(&self, context: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let found = sealed_key_version(sealed)?;
        if found != self.version {
            return Err(CryptoError::WrongKeyVersion {
                expected: self.version,
                found,
            });
        }
        open(&self.key, &self.aad(context), &sealed[5..])
    }

    /// A key derived from this one for a separate job (naming blobs), so the data key itself is
    /// used only for sealing.
    pub fn derive(&self, purpose: &str) -> Zeroizing<[u8; KEY_LEN]> {
        Zeroizing::new(blake3::derive_key(purpose, self.key.as_ref()))
    }
}

/// Which workspace key version sealed a message, to fetch that version before opening it.
pub fn sealed_key_version(sealed: &[u8]) -> Result<u32, CryptoError> {
    match sealed {
        [DATA_FORMAT_V1, a, b, c, d, ..] => Ok(u32::from_be_bytes([*a, *b, *c, *d])),
        _ => Err(CryptoError::Format),
    }
}

/// The associated data for wrapping a workspace key: which workspace and version it is.
pub(crate) fn wrap_aad(workspace: WorkspaceId, version: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(44);
    aad.extend_from_slice(b"drusite workspace key v1");
    aad.extend_from_slice(workspace.as_uuid().as_bytes());
    aad.extend_from_slice(&version.to_be_bytes());
    aad
}

/// Master keys held by Drusite itself (the `env` and `file` key sources).
///
/// Written as `id:key` entries separated by whitespace or commas, each key 32 bytes in URL-safe
/// base64. The first entry wraps new workspace keys; the others still unwrap the ones they
/// wrapped, so a master key can be rotated by putting a new one first.
pub struct LocalMasterKeys {
    keys: Vec<(String, Zeroizing<[u8; KEY_LEN]>)>,
}

impl fmt::Debug for LocalMasterKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalMasterKeys")
            .field(
                "ids",
                &self.keys.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl LocalMasterKeys {
    pub fn parse(text: &str) -> Result<Self, CryptoError> {
        let bad = |m: String| CryptoError::MasterKeys(m);
        let mut keys: Vec<(String, Zeroizing<[u8; KEY_LEN]>)> = Vec::new();
        for entry in text.split(|c: char| c == ',' || c.is_whitespace()) {
            if entry.is_empty() {
                continue;
            }
            let (id, b64) = entry
                .split_once(':')
                .ok_or_else(|| bad("each entry is id:key".to_owned()))?;
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(bad(format!(
                    "key ID {id:?} must be 1 to 64 letters, digits, - or _"
                )));
            }
            if keys.iter().any(|(existing, _)| existing == id) {
                return Err(bad(format!("key ID {id:?} appears twice")));
            }
            let raw = Zeroizing::new(
                URL_SAFE_NO_PAD
                    .decode(b64.trim_end_matches('='))
                    .or_else(|_| URL_SAFE.decode(b64))
                    .map_err(|_| bad(format!("key {id:?} isn't URL-safe base64")))?,
            );
            let key: [u8; KEY_LEN] = raw
                .as_slice()
                .try_into()
                .map_err(|_| bad(format!("key {id:?} is {} bytes, not 32", raw.len())))?;
            keys.push((id.to_owned(), Zeroizing::new(key)));
        }
        if keys.is_empty() {
            return Err(bad(
                "no master key given; make one with `drusite-server keys generate`".to_owned(),
            ));
        }
        Ok(Self { keys })
    }

    /// A new master key entry, `id:key`.
    pub fn generate_entry() -> Result<String, CryptoError> {
        let key = random_bytes::<KEY_LEN>()?;
        let id = random_bytes::<4>()?;
        let id: String = id.iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("mk-{id}:{}", URL_SAFE_NO_PAD.encode(key.as_ref())))
    }

    pub fn current_id(&self) -> &str {
        &self.keys[0].0
    }

    pub fn wrap(&self, key: &WorkspaceKey) -> Result<(String, Vec<u8>), CryptoError> {
        let (id, master) = &self.keys[0];
        let mut out = Vec::with_capacity(4 + 1 + id.len() + NONCE_LEN + KEY_LEN + TAG_LEN);
        out.extend_from_slice(WRAP_MAGIC_V1);
        out.push(u8::try_from(id.len()).map_err(|_| CryptoError::Format)?);
        out.extend_from_slice(id.as_bytes());
        seal(
            master,
            &wrap_aad(key.workspace, key.version),
            key.bytes(),
            &mut out,
        )?;
        Ok((id.clone(), out))
    }

    pub fn unwrap(
        &self,
        workspace: WorkspaceId,
        version: u32,
        wrapped: &[u8],
    ) -> Result<WorkspaceKey, CryptoError> {
        let rest = wrapped
            .strip_prefix(WRAP_MAGIC_V1.as_slice())
            .ok_or(CryptoError::Format)?;
        let (&id_len, rest) = rest.split_first().ok_or(CryptoError::Format)?;
        if rest.len() < usize::from(id_len) {
            return Err(CryptoError::Format);
        }
        let (id, sealed) = rest.split_at(usize::from(id_len));
        let id = std::str::from_utf8(id).map_err(|_| CryptoError::Format)?;
        let (_, master) = self
            .keys
            .iter()
            .find(|(k, _)| k == id)
            .ok_or_else(|| CryptoError::UnknownMasterKey(id.to_owned()))?;
        let plain = Zeroizing::new(open(master, &wrap_aad(workspace, version), sealed)?);
        let key: [u8; KEY_LEN] = plain
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Format)?;
        Ok(WorkspaceKey::from_bytes(
            workspace,
            version,
            Zeroizing::new(key),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_round_trips_and_is_bound_to_its_context() {
        let ws = WorkspaceId::new();
        let key = WorkspaceKey::generate(ws, 1).unwrap();
        let sealed = key.encrypt(b"doc-1/7", b"hello").unwrap();
        assert_eq!(key.decrypt(b"doc-1/7", &sealed).unwrap(), b"hello");
        assert_eq!(sealed_key_version(&sealed).unwrap(), 1);

        // Another context, another workspace, another version or any changed byte: refused.
        assert!(key.decrypt(b"doc-1/8", &sealed).is_err());
        let other_ws = WorkspaceKey::from_bytes(WorkspaceId::new(), 1, key.key.clone());
        assert!(other_ws.decrypt(b"doc-1/7", &sealed).is_err());
        let v2 = WorkspaceKey::from_bytes(ws, 2, key.key.clone());
        assert!(matches!(
            v2.decrypt(b"doc-1/7", &sealed),
            Err(CryptoError::WrongKeyVersion { .. })
        ));
        for i in 0..sealed.len() {
            let mut bad = sealed.clone();
            bad[i] ^= 1;
            assert!(key.decrypt(b"doc-1/7", &bad).is_err(), "flipped byte {i}");
        }
        // Nonces are random, so the same message seals differently.
        assert_ne!(sealed, key.encrypt(b"doc-1/7", b"hello").unwrap());
    }

    #[test]
    fn master_keys_wrap_and_rotate() {
        let old = LocalMasterKeys::generate_entry().unwrap();
        let new = LocalMasterKeys::generate_entry().unwrap();
        let before = LocalMasterKeys::parse(&old).unwrap();
        let ws = WorkspaceId::new();
        let key = WorkspaceKey::generate(ws, 3).unwrap();
        let (id, wrapped) = before.wrap(&key).unwrap();
        assert_eq!(id, before.current_id());
        assert!(!wrapped.windows(KEY_LEN).any(|w| w == key.bytes()));

        // After rotation the new key wraps, and the old one still unwraps what it wrapped.
        let after = LocalMasterKeys::parse(&format!("{new}, {old}")).unwrap();
        assert_ne!(after.current_id(), id);
        let back = after.unwrap(ws, 3, &wrapped).unwrap();
        assert_eq!(back.bytes(), key.bytes());

        // Bound to the workspace and version it was made for.
        assert!(after.unwrap(WorkspaceId::new(), 3, &wrapped).is_err());
        assert!(after.unwrap(ws, 4, &wrapped).is_err());
        // Without the old key it can't be opened.
        let only_new = LocalMasterKeys::parse(&new).unwrap();
        assert!(matches!(
            only_new.unwrap(ws, 3, &wrapped),
            Err(CryptoError::UnknownMasterKey(_))
        ));
    }

    #[test]
    fn master_key_parsing_rejects_mistakes() {
        assert!(LocalMasterKeys::parse("").is_err());
        assert!(LocalMasterKeys::parse("nocolon").is_err());
        assert!(LocalMasterKeys::parse("a:AAAA").is_err(), "too short");
        let entry = LocalMasterKeys::generate_entry().unwrap();
        assert!(
            LocalMasterKeys::parse(&format!("{entry} {entry}")).is_err(),
            "duplicate"
        );
        let (_, key) = entry.split_once(':').unwrap();
        assert!(LocalMasterKeys::parse(&format!("bad id:{key}")).is_err());
        // Padded base64 works too.
        let padded = format!("k1:{}=", key);
        assert!(LocalMasterKeys::parse(&padded).is_ok());
        assert!(!format!("{:?}", LocalMasterKeys::parse(&entry).unwrap()).contains(key));
    }
}
