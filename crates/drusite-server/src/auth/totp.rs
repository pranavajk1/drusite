// SPDX-License-Identifier: AGPL-3.0-or-later

//! TOTP (RFC 6238): six digits, 30-second steps, HMAC-SHA1, the parameters every
//! authenticator app supports. A code is accepted one step either side of now, and each step
//! only once.

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha1::Sha1;

pub const STEP: u64 = 30;
pub const DIGITS: u32 = 6;

/// The code for one time step (RFC 4226 §5.3).
pub fn code(secret: &[u8], step: u64) -> u32 {
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(secret) else {
        // HMAC takes keys of any length.
        return u32::MAX;
    };
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[19] & 0x0f);
    let binary = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    binary % 10u32.pow(DIGITS)
}

/// The step a code matches, if any, within one step of `unix_time`, and after `last_step`.
pub fn verify(secret: &[u8], code_text: &str, unix_time: u64, last_step: u64) -> Option<u64> {
    let code_text: String = code_text.chars().filter(|c| !c.is_whitespace()).collect();
    if code_text.len() != DIGITS as usize || !code_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let given: u32 = code_text.parse().ok()?;
    let now = unix_time / STEP;
    let mut found = None;
    // Check every candidate, so the time taken doesn't say which matched.
    for step in [now.saturating_sub(1), now, now + 1] {
        let ok = subtle::ConstantTimeEq::ct_eq(&code(secret, step), &given);
        if bool::from(ok) && step > last_step && found.is_none() {
            found = Some(step);
        }
    }
    found
}

/// RFC 4648 base32 without padding, as `otpauth://` URIs want.
pub fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// The `otpauth://` URI an authenticator app scans.
pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let enc = |s: &str| {
        url::form_urlencoded::byte_serialize(s.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    };
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP}",
        enc(issuer),
        enc(account),
        base32(secret),
        enc(issuer),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 appendix B, SHA-1, truncated to six digits.
    #[test]
    fn rfc6238_vectors() {
        let secret = b"12345678901234567890";
        for (time, expected) in [
            (59, 287_082),
            (1_111_111_109, 81_804),
            (1_111_111_111, 50_471),
            (1_234_567_890, 5_924),
            (2_000_000_000, 279_037),
            (20_000_000_000, 353_130),
        ] {
            assert_eq!(code(secret, time / STEP), expected, "{time}");
        }
    }

    #[test]
    fn codes_are_accepted_once_and_near_now() {
        let secret = b"12345678901234567890";
        assert_eq!(verify(secret, "287082", 59, 0), Some(1));
        assert_eq!(verify(secret, "287 082", 59 + 30, 0), Some(1));
        assert_eq!(verify(secret, "287082", 59 + 90, 0), None);
        // Used already.
        assert_eq!(verify(secret, "287082", 59, 1), None);
        assert_eq!(verify(secret, "28708", 59, 0), None);
        assert_eq!(verify(secret, "abcdef", 59, 0), None);
    }

    #[test]
    fn base32_rfc4648() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
    }
}
