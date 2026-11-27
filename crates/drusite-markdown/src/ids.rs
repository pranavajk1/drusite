// SPDX-License-Identifier: AGPL-3.0-or-later

//! Block IDs as written in files: 26 characters of lowercase Crockford base32, shorter than a
//! UUID's 36 and made only of what Obsidian allows in a block reference.

use drusite_model::BlockId;
use uuid::Uuid;

const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// A block ID as written in a file.
pub fn encode_block_id(id: BlockId) -> String {
    let n = id.as_uuid().as_u128();
    (0..26)
        .rev()
        .map(|i| ALPHABET[((n >> (i * 5)) & 31) as usize] as char)
        .collect()
}

/// Reads a block ID as [`encode_block_id`] writes it, or as a hyphenated UUID.
pub fn decode_block_id(s: &str) -> Option<BlockId> {
    if s.len() == 26 {
        let mut n: u128 = 0;
        for (i, c) in s.bytes().enumerate() {
            let v = ALPHABET.iter().position(|&a| a == c.to_ascii_lowercase())? as u128;
            // The first character carries only the top three bits.
            if i == 0 && v > 7 {
                return None;
            }
            n = (n << 5) | v;
        }
        return Some(BlockId::from_uuid(Uuid::from_u128(n)));
    }
    if s.len() == 36 {
        return s.parse().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for _ in 0..1000 {
            let id = BlockId::new();
            let s = encode_block_id(id);
            assert_eq!(s.len(), 26);
            assert_eq!(decode_block_id(&s), Some(id));
            assert_eq!(decode_block_id(&id.to_string()), Some(id));
        }
        assert_eq!(decode_block_id("task1"), None);
        assert_eq!(decode_block_id("zzzzzzzzzzzzzzzzzzzzzzzzzz"), None);
    }
}
