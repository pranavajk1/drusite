// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attachments over HTTP, beside the sync connection: files this device has that the server
//! doesn't go up, and files the UI asked for that aren't here come down.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use url::Url;

use crate::auth::{DeviceKey, proof};
use crate::engine::Core;

/// How many files move at once.
const AT_ONCE: usize = 4;
/// How long one file may take: long, for big files on slow networks.
const TRANSFER_WITHIN: Duration = Duration::from_secs(15 * 60);

/// How a transfer ended.
#[derive(Debug)]
pub(crate) enum Done {
    Uploaded(String),
    Downloaded(String, Vec<u8>),
    /// The server doesn't have it (or won't say).
    Missing(String),
    /// Try again later.
    Failed(String, String),
}

impl Done {
    fn blob(&self) -> &str {
        match self {
            Self::Uploaded(b) | Self::Downloaded(b, _) | Self::Missing(b) | Self::Failed(b, _) => b,
        }
    }
}

/// Transfers under way.
#[derive(Debug, Default)]
pub(crate) struct Files {
    moving: HashSet<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TRANSFER_WITHIN))
        .http_status_as_error(false)
        .build()
        .into()
}

impl Files {
    /// Starts what needs moving, up to [`AT_ONCE`] at a time. Each one's end is sent to `done`.
    pub(crate) fn start(
        &mut self,
        core: &Arc<Mutex<Core>>,
        server: &Url,
        access: &str,
        key: &(impl DeviceKey + ?Sized),
        done: &UnboundedSender<Done>,
    ) {
        let (ws, up, down) = {
            let Ok(core) = core.lock() else { return };
            (
                *core.workspace.id().as_uuid(),
                core.attachments_to_upload().unwrap_or_default(),
                core.attachments_wanted(),
            )
        };
        let work = up
            .into_iter()
            .map(|b| (b, true))
            .chain(down.into_iter().map(|b| (b, false)));
        for (blob, upload) in work {
            if self.moving.len() >= AT_ONCE {
                break;
            }
            if self.moving.contains(&blob) {
                continue;
            }
            let Ok(url) = server.join(&format!("api/v1/workspaces/{ws}/attachments/{blob}")) else {
                continue;
            };
            let method = if upload { "PUT" } else { "GET" };
            let Ok(dpop) = proof(key, method, &url, Some(access), now()) else {
                continue;
            };
            let bytes = if upload {
                match core
                    .lock()
                    .ok()
                    .and_then(|c| c.attachment(&blob).ok().flatten())
                {
                    Some(b) => Some(b),
                    None => continue,
                }
            } else {
                None
            };
            self.moving.insert(blob.clone());
            let authorization = format!("DPoP {access}");
            let done = done.clone();
            tokio::task::spawn_blocking(move || {
                let result = transfer(&url, &authorization, &dpop, blob, bytes);
                let _ = done.send(result);
            });
        }
    }

    /// Records a transfer's end in the core.
    pub(crate) fn finish(&mut self, core: &Arc<Mutex<Core>>, done: Done) {
        self.moving.remove(done.blob());
        let Ok(mut core) = core.lock() else { return };
        match done {
            Done::Uploaded(blob) => {
                let _ = core.attachment_uploaded(&blob);
            }
            Done::Downloaded(blob, bytes) => {
                if let Err(error) = core.attachment_arrived(&blob, &bytes) {
                    eprintln!("drusite: attachment {blob}: {}", error.message);
                }
            }
            Done::Missing(blob) => core.attachment_unavailable(&blob),
            Done::Failed(blob, why) => eprintln!("drusite: attachment {blob}: {why}"),
        }
    }
}

fn transfer(
    url: &Url,
    authorization: &str,
    dpop: &str,
    blob: String,
    upload: Option<Vec<u8>>,
) -> Done {
    let agent = agent();
    let response = match &upload {
        Some(bytes) => agent
            .put(url.as_str())
            .header("authorization", authorization)
            .header("dpop", dpop)
            .header("content-type", "application/octet-stream")
            .send(bytes.as_slice()),
        None => agent
            .get(url.as_str())
            .header("authorization", authorization)
            .header("dpop", dpop)
            .call(),
    };
    let mut response = match response {
        Ok(r) => r,
        Err(e) => return Done::Failed(blob, e.to_string()),
    };
    let status = response.status();
    match (upload.is_some(), status.as_u16()) {
        (true, 200..=299) => Done::Uploaded(blob),
        (false, 200..=299) => match response
            .body_mut()
            .with_config()
            .limit(u64::from(u32::MAX))
            .read_to_vec()
        {
            Ok(bytes) => Done::Downloaded(blob, bytes),
            Err(e) => Done::Failed(blob, e.to_string()),
        },
        (false, 403 | 404) => Done::Missing(blob),
        _ => Done::Failed(blob, format!("the server answered {status}")),
    }
}
