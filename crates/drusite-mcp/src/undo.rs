// SPDX-License-Identifier: AGPL-3.0-or-later

//! Undoing an agent's call: a new edit that takes back what it changed, and nothing else.

use std::collections::HashMap;

use drusite_crdt::{Page, VersionVector, Workspace};
use drusite_model::PageId;

use crate::{Activity, Change, Effect, ToolError, ToolResult};

fn version(bytes: &[u8]) -> ToolResult<VersionVector> {
    VersionVector::decode(bytes)
        .map_err(|e| ToolError::Server(format!("a recorded version doesn't decode: {e}")))
}

/// Takes back what `activity` changed in `ws` and `pages` (which must hold every page
/// [`Activity::pages`] names), and returns the changes to save. A page the call created goes to
/// the trash, where it can be restored, rather than being taken out of the tree.
pub fn undo(
    activity: &Activity,
    ws: &mut Workspace,
    pages: &mut HashMap<PageId, Page>,
    now_ms: i64,
) -> ToolResult<Change> {
    let ws_before = ws.version();
    let mut change = Change::default();
    for effect in &activity.effects {
        match effect {
            Effect::Page {
                page,
                before,
                after,
            } => {
                let doc = pages.get_mut(page).ok_or_else(|| {
                    ToolError::Server(format!("page {page} wasn't loaded for the undo"))
                })?;
                let at = doc.version();
                doc.revert_change(&version(before)?, &version(after)?)?;
                change.pages.push((*page, doc.export_updates(&at)?));
            }
            Effect::Workspace { before, after } => {
                ws.revert_change(&version(before)?, &version(after)?)?;
            }
            Effect::Created { page } => {
                if !ws.is_trashed(*page)? {
                    ws.trash_page(*page, now_ms)?;
                }
            }
        }
    }
    if ws.version() != ws_before {
        change.workspace = Some(ws.export_updates(&ws_before)?);
    }
    Ok(change)
}
