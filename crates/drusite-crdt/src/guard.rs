// SPDX-License-Identifier: AGPL-3.0-or-later

//! Containing Loro panics.
//!
//! Phase 0 found that after Loro panics, every later call on the same document panics too. So a
//! panic marks the document poisoned and every call after it fails fast with [`Error::Poisoned`]
//! until the owner reloads the document from its last saved state.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{Error, Result};

#[derive(Debug, Default)]
pub(crate) struct Guard {
    poisoned: Cell<bool>,
}

impl Guard {
    pub(crate) fn is_poisoned(&self) -> bool {
        self.poisoned.get()
    }

    /// Marks the document unusable without a panic (for a state that can't be trusted).
    pub(crate) fn poison(&self) {
        self.poisoned.set(true);
    }

    /// Runs `f`, turning a panic inside it into [`Error::Poisoned`].
    pub(crate) fn run<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.poisoned.get() {
            return Err(Error::Poisoned);
        }
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(result) => result,
            Err(_) => {
                self.poisoned.set(true);
                Err(Error::Poisoned)
            }
        }
    }
}

/// Imports `bytes` into `doc`, refusing changes that belong to another document.
///
/// A document's `meta` map is written once, when the document is created. Changes from another
/// document carry that document's own creation, which would touch `meta` again. Loro's
/// last-writer-wins could hide the rival values, so the check looks at which containers the new
/// changes touch rather than at the values.
pub(crate) fn import_checked(doc: &loro::LoroDoc, bytes: &[u8]) -> Result<()> {
    let before = doc.oplog_vv();
    doc.import(bytes)?;
    let after = doc.oplog_vv();
    let meta = loro::ContainerID::new_root("meta", loro::ContainerType::Map);
    for span in after.sub_iter(&before) {
        let len = usize::try_from(span.counter.end - span.counter.start).unwrap_or(0);
        let start = loro::ID::new(span.peer, span.counter.start);
        if doc.get_changed_containers_in(start, len).contains(&meta) {
            return Err(Error::WrongDocument);
        }
    }
    Ok(())
}
