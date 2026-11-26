// SPDX-License-Identifier: AGPL-3.0-or-later

//! The key the local store is encrypted with, and where it is kept.
//!
//! The key is 32 random bytes, handed to SQLCipher as a raw key (no passphrase, so no key
//! derivation to slow down opening). On macOS it lives in the login Keychain as a generic password,
//! not synced to iCloud Keychain; other platforms get their own key stores in later phases.
//! Losing the key means losing the store, so it is created once and never rotated silently.

use std::fmt;

use zeroize::Zeroizing;

use crate::{Error, Result};

/// A store key. Its bytes are wiped from memory when it is dropped.
#[derive(Clone)]
pub struct StoreKey(Zeroizing<[u8; 32]>);

impl StoreKey {
    /// A new random key.
    pub fn generate() -> Result<Self> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut()).map_err(|e| Error::Key(format!("no randomness: {e}")))?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The key as 64 hex digits.
    pub fn to_hex(&self) -> Zeroizing<String> {
        let mut out = Zeroizing::new(String::with_capacity(64));
        for b in self.0.iter() {
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0xf)]));
        }
        out
    }

    pub fn from_hex(hex: &str) -> Result<Self> {
        let hex = hex.trim().as_bytes();
        if hex.len() != 64 {
            return Err(Error::Key("a store key is 64 hex digits".into()));
        }
        let mut bytes = Zeroizing::new([0u8; 32]);
        for (i, pair) in hex.chunks(2).enumerate() {
            let digit = |c: u8| {
                (c as char)
                    .to_digit(16)
                    .ok_or_else(|| Error::Key("not a hex digit".into()))
            };
            bytes[i] = u8::try_from(digit(pair[0])? * 16 + digit(pair[1])?).unwrap_or(0);
        }
        Ok(Self(bytes))
    }

    /// The `PRAGMA key` value SQLCipher takes for a raw key: `"x'…'"`.
    pub(crate) fn pragma_value(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("\"x'{}'\"", self.to_hex().as_str()))
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

impl fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoreKey(…)")
    }
}

/// The macOS Keychain, as a home for store keys.
#[cfg(target_os = "macos")]
pub mod keychain {
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password, generic_password, set_generic_password_options,
    };

    use super::StoreKey;
    use crate::{Error, Result};

    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25_300;

    /// The Keychain service Drusite's keys are filed under.
    pub const SERVICE: &str = "dev.drusite.store";

    /// The key for `account` (one per store), created and saved on first use.
    pub fn load_or_create(service: &str, account: &str) -> Result<StoreKey> {
        if let Some(key) = load(service, account)? {
            return Ok(key);
        }
        let key = StoreKey::generate()?;
        let mut options = PasswordOptions::new_generic_password(service, account);
        // Only on this device (not in backups or iCloud Keychain), once unlocked after boot.
        options.set_access_synchronized(Some(false));
        options.set_label("Drusite local store key");
        set_generic_password_options(key.to_hex().as_bytes(), options).map_err(keychain_error)?;
        Ok(key)
    }

    pub fn load(service: &str, account: &str) -> Result<Option<StoreKey>> {
        let options = PasswordOptions::new_generic_password(service, account);
        match generic_password(options) {
            Ok(secret) => {
                let hex = std::str::from_utf8(&secret)
                    .map_err(|_| Error::Key("Keychain item isn't a store key".into()))?;
                Ok(Some(StoreKey::from_hex(hex)?))
            }
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(keychain_error(e)),
        }
    }

    /// Removes a key. The store it opened can never be read again.
    pub fn delete(service: &str, account: &str) -> Result<()> {
        match delete_generic_password(service, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(keychain_error(e)),
        }
    }

    fn keychain_error(e: security_framework::base::Error) -> Error {
        Error::Key(format!("Keychain: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_through_hex() {
        let key = StoreKey::generate().unwrap();
        let again = StoreKey::from_hex(&key.to_hex()).unwrap();
        assert_eq!(key.0.as_ref(), again.0.as_ref());
        assert!(StoreKey::from_hex("abc").is_err());
        assert!(StoreKey::from_hex(&"zz".repeat(32)).is_err());
        assert_eq!(format!("{key:?}"), "StoreKey(…)");
    }

    /// Writes to the login Keychain, so it only runs when asked:
    /// `DRUSITE_TEST_KEYCHAIN=1 cargo test -p drusite-core keychain`.
    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_creates_once_then_returns_the_same_key() {
        if std::env::var_os("DRUSITE_TEST_KEYCHAIN").is_none() {
            return;
        }
        let service = "dev.drusite.test";
        let account = format!("test-{}", std::process::id());
        let first = keychain::load_or_create(service, &account).unwrap();
        let second = keychain::load_or_create(service, &account).unwrap();
        assert_eq!(first.to_hex().as_str(), second.to_hex().as_str());
        keychain::delete(service, &account).unwrap();
        assert!(keychain::load(service, &account).unwrap().is_none());
    }
}
