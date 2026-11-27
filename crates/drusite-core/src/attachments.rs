// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attachments on this device: files kept in the store by the BLAKE3 hash of their content,
//! sent to the server and fetched from it by the sync driver.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use drusite_model::BlockKind;

use crate::api::{ApiError, ErrorCode, Event};
use crate::engine::Core;

/// Whether `s` is an attachment ID: 64 lowercase hex digits.
pub fn is_blob_id(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The ID of a file's content.
pub fn blob_id(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// A file name fit for a block: one line, no path.
fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let one_line: String = base.chars().filter(|c| !c.is_control()).collect();
    let trimmed = one_line.trim();
    if trimmed.is_empty() {
        "file".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// A media type from a file name, for when the caller didn't know one.
pub fn guess_mime(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

impl Core {
    /// Keeps a file and returns the attachment block for it.
    pub fn attach(&mut self, name: &str, mime: &str, bytes: &[u8]) -> Result<BlockKind, ApiError> {
        let size = u32::try_from(bytes.len())
            .map_err(|_| ApiError::new(ErrorCode::Invalid, "files over 4 GB can't be attached"))?;
        let blob = blob_id(bytes);
        self.store.put_blob(&blob, bytes, false)?;
        if let Some(notify) = &self.on_change {
            notify();
        }
        let name = clean_name(name);
        let mime = if mime.trim().is_empty() || !mime.contains('/') {
            guess_mime(&name).to_owned()
        } else {
            mime.trim().to_ascii_lowercase()
        };
        Ok(BlockKind::Attachment {
            blob,
            name,
            mime,
            size,
        })
    }

    pub(crate) fn add_attachment(
        &mut self,
        name: &str,
        mime: &str,
        data: &str,
    ) -> Result<BlockKind, ApiError> {
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| ApiError::new(ErrorCode::Invalid, "the file isn't base64"))?;
        self.attach(name, mime, &bytes)
    }

    /// A file's bytes, if it's on this device.
    pub fn attachment(&self, blob: &str) -> Result<Option<Vec<u8>>, ApiError> {
        Ok(self.store.blob(blob)?)
    }

    pub(crate) fn read_attachment(&mut self, blob: &str) -> Result<String, ApiError> {
        if !is_blob_id(blob) {
            return Err(ApiError::new(ErrorCode::Invalid, "not an attachment ID"));
        }
        if let Some(bytes) = self.store.blob(blob)? {
            return Ok(STANDARD.encode(bytes));
        }
        if self.wanted_blobs.insert(blob.to_owned())
            && let Some(notify) = &self.on_change
        {
            notify();
        }
        Err(ApiError::new(
            ErrorCode::NotHere,
            "the file is on its way from the server",
        ))
    }

    /// Files to send to the server.
    pub fn attachments_to_upload(&self) -> Result<Vec<String>, ApiError> {
        Ok(self.store.blobs_to_upload()?)
    }

    pub fn attachment_uploaded(&mut self, blob: &str) -> Result<(), ApiError> {
        Ok(self.store.blob_uploaded(blob)?)
    }

    /// Files to fetch from the server: asked for and not here.
    pub fn attachments_wanted(&self) -> Vec<String> {
        self.wanted_blobs.iter().cloned().collect()
    }

    /// A file fetched from the server. Refused if it isn't the file `blob` names.
    pub fn attachment_arrived(&mut self, blob: &str, bytes: &[u8]) -> Result<(), ApiError> {
        self.wanted_blobs.remove(blob);
        if blob_id(bytes) != blob {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "the server sent a different file",
            ));
        }
        self.store.put_blob(blob, bytes, true)?;
        (self.events)(Event::AttachmentArrived {
            blob: blob.to_owned(),
        });
        Ok(())
    }

    /// Gives up on fetching a file (the server doesn't have it).
    pub fn attachment_unavailable(&mut self, blob: &str) {
        self.wanted_blobs.remove(blob);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use drusite_model::BlockKind;

    use crate::api::{Command, ErrorCode, Response};
    use crate::{Core, StoreKey};

    #[test]
    fn files_are_kept_by_their_hash_and_queued_for_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let mut core =
            Core::open(dir.path(), &StoreKey::generate().unwrap(), Box::new(|_| {})).unwrap();
        let Response::Attachment { kind } = core
            .handle(Command::AddAttachment {
                name: "../Photos/beach.JPG".into(),
                mime: String::new(),
                data: STANDARD.encode(b"jpeg bytes"),
            })
            .unwrap()
        else {
            panic!()
        };
        let BlockKind::Attachment {
            blob,
            name,
            mime,
            size,
        } = kind
        else {
            panic!()
        };
        assert_eq!(blob, blake3::hash(b"jpeg bytes").to_hex().as_str());
        assert_eq!(
            (name.as_str(), mime.as_str(), size),
            ("beach.JPG", "image/jpeg", 10)
        );
        let Response::AttachmentData { data } = core
            .handle(Command::ReadAttachment { blob: blob.clone() })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(STANDARD.decode(data).unwrap(), b"jpeg bytes");
        assert_eq!(
            core.attachments_to_upload().unwrap(),
            std::slice::from_ref(&blob)
        );
        core.attachment_uploaded(&blob).unwrap();
        assert!(core.attachments_to_upload().unwrap().is_empty());

        // One that isn't here is wanted, and arrives only if it's the right file.
        let other = blake3::hash(b"other").to_hex().to_string();
        let err = core
            .handle(Command::ReadAttachment {
                blob: other.clone(),
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotHere);
        assert_eq!(core.attachments_wanted(), std::slice::from_ref(&other));
        assert!(core.attachment_arrived(&other, b"not it").is_err());
        core.attachment_arrived(&other, b"other").unwrap();
        assert!(core.attachments_wanted().is_empty());
        assert!(
            core.attachments_to_upload().unwrap().is_empty(),
            "came from the server"
        );
    }
}
