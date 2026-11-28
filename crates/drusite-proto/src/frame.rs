// SPDX-License-Identifier: AGPL-3.0-or-later

//! Frames: one WebSocket binary message each. The first byte says how the rest is encoded:
//! `0` plain Protobuf, `1` zstd-compressed Protobuf. Big messages (catch-ups, pasted pages) are
//! compressed; small ones (keystrokes) aren't worth it.

use prost::Message;

use crate::MAX_FRAME_BYTES;

const PLAIN: u8 = 0;
const ZSTD: u8 = 1;
/// Messages this big or bigger are compressed.
#[cfg(feature = "zstd")]
const COMPRESS_FROM: usize = 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("empty frame")]
    Empty,
    #[error("unknown frame encoding {0}")]
    Encoding(u8),
    #[error("frame is bigger than {MAX_FRAME_BYTES} bytes")]
    TooLarge,
    #[error("frame doesn't decompress")]
    Decompress,
    #[error("frame isn't a valid message: {0}")]
    Decode(String),
    #[error("this build can't read compressed frames")]
    NoZstd,
}

/// Encodes a message into a frame.
pub fn encode(message: &impl Message) -> Vec<u8> {
    let body = message.encode_to_vec();
    #[cfg(feature = "zstd")]
    if body.len() >= COMPRESS_FROM
        && let Ok(compressed) = zstd::bulk::compress(&body, 3)
        && compressed.len() < body.len()
    {
        let mut frame = Vec::with_capacity(compressed.len() + 1);
        frame.push(ZSTD);
        frame.extend(compressed);
        return frame;
    }
    let mut frame = Vec::with_capacity(body.len() + 1);
    frame.push(PLAIN);
    frame.extend(body);
    frame
}

/// Decodes a frame. Refuses anything that is, or decompresses to, more than
/// [`MAX_FRAME_BYTES`].
pub fn decode<M: Message + Default>(frame: &[u8]) -> Result<M, FrameError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let (&encoding, body) = frame.split_first().ok_or(FrameError::Empty)?;
    let decoded = match encoding {
        PLAIN => M::decode(body),
        ZSTD => {
            #[cfg(feature = "zstd")]
            {
                let plain = zstd::bulk::decompress(body, MAX_FRAME_BYTES)
                    .map_err(|_| FrameError::Decompress)?;
                M::decode(plain.as_slice())
            }
            #[cfg(all(not(feature = "zstd"), feature = "unzstd"))]
            {
                use std::io::Read as _;
                let decoder = ruzstd::decoding::StreamingDecoder::new(body)
                    .map_err(|_| FrameError::Decompress)?;
                let mut plain = Vec::new();
                decoder
                    .take(MAX_FRAME_BYTES as u64 + 1)
                    .read_to_end(&mut plain)
                    .map_err(|_| FrameError::Decompress)?;
                if plain.len() > MAX_FRAME_BYTES {
                    return Err(FrameError::TooLarge);
                }
                M::decode(plain.as_slice())
            }
            #[cfg(not(any(feature = "zstd", feature = "unzstd")))]
            return Err(FrameError::NoZstd);
        }
        other => return Err(FrameError::Encoding(other)),
    };
    decoded.map_err(|e| FrameError::Decode(e.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{ClientMessage, Push, client_message::Body};

    fn push(len: usize) -> ClientMessage {
        ClientMessage::new(Body::Push(Push {
            doc: vec![7; 16],
            push_id: 42,
            update: vec![b'a'; len],
        }))
    }

    /// The browser build: ruzstd reads what the server's zstd writes, and still refuses bombs.
    #[test]
    #[cfg(all(feature = "unzstd", not(feature = "zstd")))]
    fn ruzstd_reads_compressed_frames() {
        let message = push(100_000);
        let mut frame = vec![ZSTD];
        frame.extend(zstd::bulk::compress(&message.encode_to_vec(), 3).unwrap());
        assert_eq!(decode::<ClientMessage>(&frame).unwrap(), message);
        // What this build writes is plain.
        assert_eq!(encode(&message)[0], PLAIN);
        let mut bomb = vec![ZSTD];
        bomb.extend(zstd::bulk::compress(&vec![0u8; 32 * 1024 * 1024], 19).unwrap());
        assert_eq!(decode::<ClientMessage>(&bomb), Err(FrameError::TooLarge));
    }

    #[test]
    #[cfg(feature = "zstd")]
    fn small_frames_are_plain_and_big_ones_compressed() {
        let small = encode(&push(10));
        assert_eq!(small[0], PLAIN);
        assert_eq!(decode::<ClientMessage>(&small).unwrap(), push(10));
        let big = encode(&push(100_000));
        assert_eq!(big[0], ZSTD);
        assert!(big.len() < 1000);
        assert_eq!(decode::<ClientMessage>(&big).unwrap(), push(100_000));
    }

    #[test]
    #[cfg(feature = "zstd")]
    fn bad_frames_are_errors_not_panics() {
        assert_eq!(decode::<ClientMessage>(&[]), Err(FrameError::Empty));
        assert_eq!(
            decode::<ClientMessage>(&[9, 1]),
            Err(FrameError::Encoding(9))
        );
        assert!(matches!(
            decode::<ClientMessage>(&[PLAIN, 0xff, 0xff]),
            Err(FrameError::Decode(_))
        ));
        assert_eq!(
            decode::<ClientMessage>(&[ZSTD, 1, 2, 3]),
            Err(FrameError::Decompress)
        );
        // A bomb: 32 MiB of zeros compresses to almost nothing, but won't be inflated.
        let mut bomb = vec![ZSTD];
        bomb.extend(zstd::bulk::compress(&vec![0u8; 32 * 1024 * 1024], 19).unwrap());
        assert!(bomb.len() < 10_000);
        assert_eq!(decode::<ClientMessage>(&bomb), Err(FrameError::Decompress));
    }

    #[test]
    fn unknown_fields_are_skipped() {
        // A newer peer's message with an extra field (tag 15) still decodes.
        let mut bytes = push(3).encode_to_vec();
        bytes.extend([0x78, 0x01]); // field 15, varint 1
        let mut frame = vec![PLAIN];
        frame.extend(bytes);
        assert_eq!(decode::<ClientMessage>(&frame).unwrap(), push(3));
    }
}
