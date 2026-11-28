// SPDX-License-Identifier: AGPL-3.0-or-later

//! The trash: deleted pages wait here, restorable, for thirty days, then go for good.

use drusite_model::PageId;

use crate::api::{ApiError, TrashItem};
use crate::engine::Core;

/// How long a page stays in the trash before it is deleted for good.
pub(crate) const KEEP_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Now, in Unix milliseconds.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Now, in Unix milliseconds: the browser's clock (`SystemTime` panics on wasm32).
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub(crate) fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

impl Core {
    pub(crate) fn trash(&self) -> Result<Vec<TrashItem>, ApiError> {
        Ok(self
            .workspace
            .trash()?
            .into_iter()
            .map(|t| TrashItem {
                id: t.id,
                title: t.title,
                parent: t.parent,
                trashed_at: t.trashed_at as f64,
                deleted_at: t.trashed_at.saturating_add(KEEP_MS) as f64,
                subpages: u32::try_from(t.subpages).unwrap_or(u32::MAX),
            })
            .collect())
    }

    /// Deletes pages, and their subpages, for good: out of the tree and out of the store.
    pub(crate) fn delete_forever(&mut self, pages: &[PageId]) -> Result<(), ApiError> {
        if pages.is_empty() {
            return Ok(());
        }
        let mut gone = Vec::new();
        for &page in pages {
            // A page already gone with an earlier one in the list.
            if self.workspace.title(page).is_ok() {
                gone.extend(self.workspace.delete_page(page)?);
            }
        }
        self.save_workspace()?;
        for page in gone {
            self.open.remove(&page);
            self.search_changed(*page.as_uuid());
            self.store.delete_doc(&page.to_string())?;
        }
        Ok(())
    }

    /// Deletes for good what has been in the trash longer than [`KEEP_MS`].
    pub(crate) fn purge_trash(&mut self, now: i64) -> Result<(), ApiError> {
        let old: Vec<PageId> = self
            .workspace
            .trash()?
            .into_iter()
            .filter(|t| now.saturating_sub(t.trashed_at) > KEEP_MS)
            .map(|t| t.id)
            .collect();
        self.delete_forever(&old)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use crate::api::{Command, Edit, Response};
    use crate::{Core, StoreKey};

    use super::KEEP_MS;

    fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let core =
            Core::open(dir.path(), &StoreKey::generate().unwrap(), Box::new(|_| {})).unwrap();
        (core, dir)
    }

    fn page(
        core: &mut Core,
        parent: Option<drusite_model::PageId>,
        title: &str,
    ) -> drusite_model::PageId {
        let Response::PageCreated { page } = core
            .handle(Command::CreatePage {
                parent,
                title: title.into(),
            })
            .unwrap()
        else {
            panic!()
        };
        page.id
    }

    fn titles(core: &mut Core) -> Vec<String> {
        let Response::Pages { pages } = core.handle(Command::ListPages).unwrap() else {
            panic!()
        };
        pages.into_iter().map(|p| p.title).collect()
    }

    fn trash(core: &mut Core) -> Vec<String> {
        let Response::Trash { pages } = core.handle(Command::Trash).unwrap() else {
            panic!()
        };
        pages.into_iter().map(|p| p.title).collect()
    }

    #[test]
    fn deleting_moves_to_the_trash_and_restoring_brings_back_the_text() {
        let (mut core, _dir) = core();
        let notes = page(&mut core, None, "Notes");
        page(&mut core, Some(notes), "Sub");
        let Response::Opened { view } = core.handle(Command::OpenPage { page: notes }).unwrap()
        else {
            panic!()
        };
        core.handle(Command::Edit {
            page: notes,
            edits: vec![Edit::InsertText {
                block: view.blocks[0].id,
                pos: 0,
                text: "keep me".into(),
            }],
        })
        .unwrap();
        core.handle(Command::ClosePage { page: notes }).unwrap();

        core.handle(Command::DeletePage { page: notes }).unwrap();
        assert!(titles(&mut core).is_empty());
        assert_eq!(trash(&mut core), ["Notes"]);
        let Response::Trash { pages } = core.handle(Command::Trash).unwrap() else {
            panic!()
        };
        assert_eq!(pages[0].subpages, 1);
        assert_eq!(pages[0].deleted_at - pages[0].trashed_at, KEEP_MS as f64);

        core.handle(Command::RestorePage { page: notes }).unwrap();
        assert_eq!(titles(&mut core), ["Notes", "Sub"]);
        let Response::Opened { view } = core.handle(Command::OpenPage { page: notes }).unwrap()
        else {
            panic!()
        };
        assert_eq!(view.blocks[0].text(), "keep me");
    }

    #[test]
    fn deleting_forever_removes_the_documents_and_only_from_the_trash() {
        let (mut core, _dir) = core();
        let a = page(&mut core, None, "A");
        let child = page(&mut core, Some(a), "Child");
        assert!(core.handle(Command::DeleteForever { page: a }).is_err());
        core.handle(Command::DeletePage { page: a }).unwrap();
        core.handle(Command::DeleteForever { page: a }).unwrap();
        assert!(trash(&mut core).is_empty());
        assert!(core.store.load(&a.to_string()).unwrap().is_none());
        assert!(core.store.load(&child.to_string()).unwrap().is_none());
        assert!(core.handle(Command::RestorePage { page: a }).is_err());
    }

    #[test]
    fn emptying_and_the_thirty_day_limit() {
        let (mut core, _dir) = core();
        let a = page(&mut core, None, "A");
        let b = page(&mut core, None, "B");
        core.handle(Command::DeletePage { page: a }).unwrap();
        core.handle(Command::DeletePage { page: b }).unwrap();
        let now = super::now_ms();
        core.purge_trash(now + KEEP_MS - 60_000).unwrap();
        assert_eq!(trash(&mut core).len(), 2, "not yet thirty days");
        core.purge_trash(now + KEEP_MS + 60_000).unwrap();
        assert!(trash(&mut core).is_empty());

        let c = page(&mut core, None, "C");
        core.handle(Command::DeletePage { page: c }).unwrap();
        core.handle(Command::EmptyTrash).unwrap();
        assert!(trash(&mut core).is_empty());
        assert!(core.store.load(&c.to_string()).unwrap().is_none());
    }
}
