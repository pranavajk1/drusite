// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bearer secrets: session cookies, codes, refresh and access tokens, magic links.
//!
//! Each is 32 random bytes in base64url behind a short prefix that says what it is, so a
//! secret scanner can spot one in a log or a paste. Only the BLAKE3 hash of a secret is stored.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::crypto::{CryptoError, random_bytes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Session,
    Code,
    Refresh,
    Access,
    MagicLink,
}

impl Kind {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Session => "drs_",
            Self::Code => "drc_",
            Self::Refresh => "drr_",
            Self::Access => "dra_",
            Self::MagicLink => "drm_",
        }
    }
}

/// A new secret of `kind`.
pub(super) fn generate(kind: Kind) -> Result<String, CryptoError> {
    let raw = random_bytes::<32>()?;
    Ok(format!(
        "{}{}",
        kind.prefix(),
        URL_SAFE_NO_PAD.encode(raw.as_ref())
    ))
}

/// The stored form of a secret. A secret of another kind, or one that isn't a secret at all,
/// has no hash, so it can't match anything.
pub(super) fn hash(kind: Kind, secret: &str) -> Option<[u8; 32]> {
    let body = secret.strip_prefix(kind.prefix())?;
    (body.len() == 43
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| *blake3::hash(secret.as_bytes()).as_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn secrets_hash_only_as_their_own_kind() {
        let s = generate(Kind::Refresh).unwrap();
        assert!(s.starts_with("drr_"));
        assert!(hash(Kind::Refresh, &s).is_some());
        assert!(hash(Kind::Access, &s).is_none());
        assert!(hash(Kind::Refresh, "drr_short").is_none());
        assert_ne!(generate(Kind::Refresh).unwrap(), s);
    }
}
