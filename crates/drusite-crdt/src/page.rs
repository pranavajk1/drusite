// SPDX-License-Identifier: AGPL-3.0-or-later

//! A page document: its blocks, and every edit the editor can make to them.
//!
//! Positions in the API are UTF-16 code units within one block's text, the unit ProseMirror and
//! the browser use. They are checked and converted before anything is written.

use drusite_model::{Block, BlockId, BlockKind, MAX_INDENT, Mark, PageId, SCHEMA_VERSION, Span};
use loro::cursor::PosType;
use loro::{
    ExportMode, LoroDoc, LoroMap, LoroText, LoroValue, TextDelta, ValueOrContainer, VersionVector,
};

use crate::attrs::{self, BLOCK_STYLE};
use crate::guard::Guard;
use crate::layout::Layout;
use crate::{Error, MARKER, Result};

/// What an edit changed, so the editor redraws only that.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patch {
    /// Blocks whose text, kind or indent changed, or that are new.
    pub updated: Vec<BlockId>,
    /// Blocks that are gone.
    pub removed: Vec<BlockId>,
    /// Whether the display order changed (blocks added, removed or moved).
    pub order_changed: bool,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.updated.is_empty() && self.removed.is_empty() && !self.order_changed
    }
}

/// How [`Page::join`] merged a block into the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinOutcome {
    /// The marker between them was deleted: no text moved.
    Merged,
    /// The two weren't next to each other in the body (one had been moved), so the block's text
    /// was copied onto the end of the previous one and the block deleted. If two devices both
    /// do this to the same block before they sync, its text shows up twice: duplicated, never
    /// lost.
    Copied,
    /// The block is the first one shown; there is nothing to join it to.
    Nothing,
}

/// One page's Loro document. See the crate docs for how it is laid out.
#[derive(Debug)]
pub struct Page {
    guard: Guard,
    inner: Inner,
}

#[derive(Debug)]
struct Inner {
    doc: LoroDoc,
    id: PageId,
    genesis: BlockId,
    layout: Layout,
}

impl Page {
    /// Creates a new page with one empty paragraph. This is the only way a page comes into
    /// being; every other copy of it is loaded from this one's changes.
    pub fn create() -> Result<Self> {
        let doc = LoroDoc::new();
        attrs::configure_styles(&doc);
        let id = PageId::new();
        let genesis = BlockId::new();
        let meta = doc.get_map("meta");
        meta.insert("schema", i64::from(SCHEMA_VERSION))?;
        meta.insert("page", id.to_string())?;
        meta.insert("genesis", genesis.to_string())?;
        let body = doc.get_text("body");
        body.insert(0, &MARKER.to_string())?;
        body.mark(0..1, BLOCK_STYLE, genesis.to_string())?;
        let block = doc
            .get_map("blocks")
            .insert_container(&genesis.to_string(), LoroMap::new())?;
        attrs::write_kind(&block, &BlockKind::Paragraph)?;
        doc.get_movable_list("order").push(genesis.to_string())?;
        doc.commit();
        let layout = Layout::compute(&doc, Some(genesis));
        Ok(Self {
            guard: Guard::default(),
            inner: Inner {
                doc,
                id,
                genesis,
                layout,
            },
        })
    }

    /// Loads a page from a snapshot or from updates (anything [`Page::export_snapshot`] or
    /// [`Page::export_updates`] produced, starting from the page's creation).
    pub fn load(bytes: &[u8]) -> Result<Self> {
        Self::load_all(&[bytes])
    }

    /// Loads a page from a snapshot followed by the updates logged after it.
    pub fn load_all(parts: &[&[u8]]) -> Result<Self> {
        let guard = Guard::default();
        let inner = guard.run(|| {
            let doc = LoroDoc::new();
            attrs::configure_styles(&doc);
            for part in parts {
                doc.import(part)?;
            }
            let meta = doc.get_map("meta");
            let not_page = || Error::NotADocument { expected: "page" };
            let schema = u32::try_from(attrs::get_int(&meta, "schema")).map_err(|_| not_page())?;
            if schema == 0 {
                return Err(not_page());
            }
            if schema > SCHEMA_VERSION {
                return Err(Error::NewerSchema {
                    found: schema,
                    supported: SCHEMA_VERSION,
                });
            }
            let id: PageId = attrs::get_str(&meta, "page")
                .and_then(|s| s.parse().ok())
                .ok_or_else(not_page)?;
            let genesis: BlockId = attrs::get_str(&meta, "genesis")
                .and_then(|s| s.parse().ok())
                .ok_or_else(not_page)?;
            let layout = Layout::compute(&doc, Some(genesis));
            Ok(Inner {
                doc,
                id,
                genesis,
                layout,
            })
        })?;
        Ok(Self { guard, inner })
    }

    pub fn id(&self) -> PageId {
        self.inner.id
    }

    /// A copy of the page as it was at `version`, writing as a new peer. Edits made to the copy
    /// are concurrent with everything this page has had since `version`: exporting them (from
    /// `version`) and importing them here merges them, as if another device had made them then.
    /// That is how a Markdown file edited on disk comes back without losing what changed in
    /// the app meanwhile.
    pub fn fork_at(&self, version: &VersionVector) -> Result<Self> {
        let guard = Guard::default();
        let inner = guard.run(|| {
            if !self.inner.doc.oplog_vv().includes_vv(version) {
                return Err(Error::Invalid(
                    "that version isn't in this page's history".into(),
                ));
            }
            let frontiers = self.inner.doc.vv_to_frontiers(version);
            let doc = self.inner.doc.fork_at(&frontiers)?;
            attrs::configure_styles(&doc);
            // Never the original's peer: its counters there may already be taken. A fresh one
            // from the top of the range: where the copy and the page inserted at the same spot,
            // Loro puts the lower peer's insert first, so text typed in the app at the end of a
            // block stays in it when the copy adds a block after it.
            doc.set_peer_id(u64::MAX - (LoroDoc::new().peer_id() >> 16))?;
            let layout = Layout::compute(&doc, Some(self.inner.genesis));
            Ok(Inner {
                doc,
                id: self.inner.id,
                genesis: self.inner.genesis,
                layout,
            })
        })?;
        Ok(Self { guard, inner })
    }

    /// Whether Loro panicked inside this page. A poisoned page refuses every call; reload it.
    pub fn is_poisoned(&self) -> bool {
        self.guard.is_poisoned()
    }

    /// The blocks in display order.
    pub fn order(&self) -> Vec<BlockId> {
        self.inner.layout.display.clone()
    }

    /// Where block `id` is shown, counting from 0, if it is shown.
    pub fn position(&self, id: BlockId) -> Option<usize> {
        self.inner.layout.display_at(id)
    }

    /// The block shown just before `id` (`None` for the first block, or one not shown).
    pub fn block_before(&self, id: BlockId) -> Option<BlockId> {
        self.inner.layout.display_prev(id)
    }

    /// Every block, in display order.
    pub fn blocks(&self) -> Result<Vec<Block>> {
        self.guard.run(|| {
            self.inner
                .layout
                .display
                .iter()
                .map(|&id| self.inner.block(id))
                .collect()
        })
    }

    pub fn block(&self, id: BlockId) -> Result<Block> {
        self.guard.run(|| self.inner.block(id))
    }

    /// A block's text length in UTF-16 code units.
    pub fn text_len(&self, id: BlockId) -> Result<usize> {
        self.guard.run(|| Ok(utf16_len(&self.inner.raw_text(id)?)))
    }

    /// Fills a new page with copies of `blocks` (a template's), each with a new ID, with
    /// `map_text` applied to every span's text. The page's empty first block becomes the first
    /// copy. Meant for a page nobody has edited yet.
    pub fn fill_from(&mut self, blocks: &[Block], map_text: impl Fn(&str) -> String) -> Result<()> {
        let mut prev: Option<BlockId> = None;
        for (i, block) in blocks.iter().enumerate() {
            let id = match (i, self.order().as_slice()) {
                (0, [only]) if self.text_len(*only)? == 0 => {
                    self.set_kind(*only, &block.kind)?;
                    *only
                }
                _ => self.insert_block_after(prev, block.kind.clone())?.0,
            };
            let mut pos = 0;
            for span in &block.spans {
                let text = map_text(&span.text);
                let len = text.encode_utf16().count();
                self.insert_text(id, pos, &text)?;
                for mark in marks_of(&span.marks) {
                    self.format(id, pos, pos + len, &mark, true)?;
                }
                pos += len;
            }
            if block.indent > 0 {
                self.set_indent(id, block.indent)?;
            }
            prev = Some(id);
        }
        Ok(())
    }

    /// Types `text` into block `id` at `pos`.
    pub fn insert_text(&mut self, id: BlockId, pos: usize, text: &str) -> Result<Patch> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            let text = sanitize(text);
            if text.is_empty() {
                return Ok(Patch::default());
            }
            let at = inner.body_pos(id, pos)?;
            inner.body().insert(at, &text)?;
            inner.layout.shift_after(id, count_isize(&text));
            if id == inner.genesis && inner.is_hidden(id) {
                inner.set_attr(id, "hidden", false)?;
                let before = inner.layout.clone();
                return inner.finish(before, vec![id]);
            }
            inner.doc.commit();
            Ok(Patch {
                updated: vec![id],
                ..Patch::default()
            })
        })
    }

    /// Deletes `len` UTF-16 code units of block `id`'s text from `pos`.
    pub fn delete_text(&mut self, id: BlockId, pos: usize, len: usize) -> Result<Patch> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            if len == 0 {
                return Ok(Patch::default());
            }
            let end = pos.checked_add(len).ok_or(Error::OutOfRange {
                pos: usize::MAX,
                len: 0,
            })?;
            let from = inner.body_pos(id, pos)?;
            let to = inner.body_pos(id, end)?;
            inner.body().delete(from, to - from)?;
            inner
                .layout
                .shift_after(id, -isize::try_from(to - from).unwrap_or(isize::MAX));
            if id == inner.genesis && inner.is_hidden(id) {
                // A deleted first block that had text typed into it hides again once it's empty.
                let before = inner.layout.clone();
                return inner.finish(before, vec![id]);
            }
            inner.doc.commit();
            Ok(Patch {
                updated: vec![id],
                ..Patch::default()
            })
        })
    }

    /// Adds (`on`) or removes formatting on block `id`'s text from `start` to `end`.
    pub fn format(
        &mut self,
        id: BlockId,
        start: usize,
        end: usize,
        mark: &Mark,
        on: bool,
    ) -> Result<Patch> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            if start >= end {
                return Ok(Patch::default());
            }
            let from = inner.body_pos(id, start)?;
            let to = inner.body_pos(id, end)?;
            if on {
                inner
                    .body()
                    .mark(from..to, mark.key(), attrs::mark_value(mark))?;
            } else {
                inner.body().unmark(from..to, mark.key())?;
            }
            inner.doc.commit();
            Ok(Patch {
                updated: vec![id],
                ..Patch::default()
            })
        })
    }

    /// Splits block `id` at `pos` (Enter). The text before `pos` stays in `id`; the text after it
    /// goes to a new block of kind `kind`, shown right after it at the same indent.
    pub fn split(&mut self, id: BlockId, pos: usize, kind: BlockKind) -> Result<(BlockId, Patch)> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            let at = inner.body_pos(id, pos)?;
            let new = inner.insert_marker(id, at, &kind, true)?;
            inner.doc.commit();
            Ok((
                new,
                Patch {
                    updated: vec![id, new],
                    removed: vec![],
                    order_changed: true,
                },
            ))
        })
    }

    /// Adds an empty block of kind `kind` after `after`, or at the top of the page.
    pub fn insert_block_after(
        &mut self,
        after: Option<BlockId>,
        kind: BlockKind,
    ) -> Result<(BlockId, Patch)> {
        let inner = &mut self.inner;
        self.guard.run(|| match after {
            Some(a) => {
                let (_, end) = inner.layout.text_range(a).ok_or(Error::UnknownBlock(a))?;
                let new = inner.insert_marker(a, end, &kind, true)?;
                inner.doc.commit();
                Ok((
                    new,
                    Patch {
                        updated: vec![new],
                        removed: vec![],
                        order_changed: true,
                    },
                ))
            }
            None => {
                let before = inner.layout.clone();
                let last = *inner
                    .layout
                    .body
                    .last()
                    .ok_or(Error::UnknownBlock(inner.genesis))?;
                let end = inner.layout.body_len;
                // The new block only sits after `last` in the text; nobody worked in `last`, so a
                // deleted first block there stays deleted.
                let new = inner.insert_marker(last, end, &kind, false)?;
                inner.order().insert(0, new.to_string())?;
                Ok((new, inner.finish(before, vec![new])?))
            }
        })
    }

    /// Joins block `id` onto the end of the block shown before it (Backspace at the start of a
    /// block). The joined text keeps the previous block's kind.
    pub fn join(&mut self, id: BlockId) -> Result<(JoinOutcome, Patch)> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            inner.check(id)?;
            let Some(prev) = inner.layout.display_prev(id) else {
                return Ok((JoinOutcome::Nothing, Patch::default()));
            };
            if id != inner.genesis && inner.layout.body_prev(id) == Some(prev) {
                // The texts are already next to each other: drop the marker between them.
                let i = inner.layout.body_at(id).ok_or(Error::UnknownBlock(id))?;
                inner.body().delete(inner.layout.marker_pos[i], 1)?;
                inner.forget(id)?;
                let doc = &inner.doc;
                let reindented = inner.layout.remove_joined(id, |b| stored_indent(doc, b));
                inner.doc.commit();
                let mut updated = vec![prev];
                updated.extend(reindented.into_iter().filter(|&b| b != prev));
                return Ok((
                    JoinOutcome::Merged,
                    Patch {
                        updated,
                        removed: vec![id],
                        order_changed: true,
                    },
                ));
            }
            // One of them was moved, so their texts are apart: copy, then delete.
            let before = inner.layout.clone();
            let spans = inner.block(id)?.spans;
            let (_, mut at) = inner
                .layout
                .text_range(prev)
                .ok_or(Error::UnknownBlock(prev))?;
            for span in spans {
                let n = span.text.chars().count();
                inner.body().insert(at, &span.text)?;
                // Copied text keeps its own formatting, not what it would inherit from the text
                // it lands after.
                let marks = marks_of(&span.marks);
                for key in INLINE_KEYS {
                    match marks.iter().find(|m| m.key() == key) {
                        Some(mark) => {
                            inner
                                .body()
                                .mark(at..at + n, key, attrs::mark_value(mark))?
                        }
                        None => inner.body().unmark(at..at + n, key)?,
                    }
                }
                at += n;
            }
            inner.layout = Layout::compute(&inner.doc, Some(inner.genesis));
            inner.delete_block(id)?;
            Ok((JoinOutcome::Copied, inner.finish(before, vec![prev])?))
        })
    }

    /// Deletes block `id` and its text.
    pub fn delete_block(&mut self, id: BlockId) -> Result<Patch> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            let before = inner.layout.clone();
            inner.delete_block(id)?;
            inner.finish(before, vec![])
        })
    }

    /// Changes what block `id` is (paragraph, heading, to-do, …). Its text and ID stay.
    pub fn set_kind(&mut self, id: BlockId, kind: &BlockKind) -> Result<Patch> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            inner.check(id)?;
            let map = inner.attrs_map(id)?;
            attrs::write_kind(&map, kind)?;
            // A kind is shown inside the block only; nothing else moves.
            inner.doc.commit();
            Ok(Patch {
                updated: vec![id],
                ..Patch::default()
            })
        })
    }

    /// Sets block `id`'s indent. It can't go more than one level deeper than the block before it.
    pub fn set_indent(&mut self, id: BlockId, indent: u8) -> Result<Patch> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            inner.check(id)?;
            let limit = inner
                .layout
                .display_prev(id)
                .map_or(0, |p| inner.shown_indent(p).saturating_add(1));
            let indent = indent.min(limit).min(MAX_INDENT);
            let before = inner.layout.clone();
            inner.set_attr(id, "indent", i64::from(indent))?;
            inner.finish(before, vec![id])
        })
    }

    /// Moves block `id` to just after block `after`, or to the top of the page.
    pub fn move_block(&mut self, id: BlockId, after: Option<BlockId>) -> Result<Patch> {
        let inner = &mut self.inner;
        self.guard.run(|| {
            inner.check(id)?;
            if let Some(a) = after {
                inner.check(a)?;
            }
            if after == Some(id) || inner.layout.display_prev(id) == after {
                return Ok(Patch::default());
            }
            let before = inner.layout.clone();
            // The block after `id` in the body would otherwise follow it to its new place.
            inner.pin_next(id)?;
            let target = match after {
                Some(a) => {
                    // So `id` lands right after `a`, not after the rest of `a`'s group.
                    if inner.layout.body_next(a) != Some(id) {
                        inner.pin_next(a)?;
                    }
                    let head = inner.layout.head(a).ok_or(Error::UnknownBlock(a))?;
                    inner.order_index(head).map_or(0, |i| i + 1)
                }
                None => 0,
            };
            let order = inner.order();
            match inner.order_index(id) {
                Some(from) => {
                    let to = if from < target { target - 1 } else { target };
                    if from != to {
                        order.mov(from, to)?;
                    }
                }
                None => order.insert(target, id.to_string())?,
            }
            inner.finish(before, vec![id])
        })
    }

    /// Applies changes made elsewhere (another device, or the saved log).
    pub fn import(&mut self, bytes: &[u8]) -> Result<Patch> {
        let inner = &mut self.inner;
        let result = self.guard.run(|| {
            let before: Vec<Block> = inner
                .layout
                .display
                .iter()
                .map(|&id| inner.block(id))
                .collect::<Result<_>>()?;
            crate::guard::import_checked(&inner.doc, bytes)?;
            inner.layout = Layout::compute(&inner.doc, Some(inner.genesis));
            let mut patch = Patch::default();
            let old: std::collections::HashMap<BlockId, &Block> =
                before.iter().map(|b| (b.id, b)).collect();
            for &id in &inner.layout.display {
                let block = inner.block(id)?;
                if old.get(&id).is_none_or(|b| **b != block) {
                    patch.updated.push(id);
                }
            }
            patch.removed = before
                .iter()
                .map(|b| b.id)
                .filter(|id| inner.layout.display_at(*id).is_none())
                .collect();
            patch.order_changed = before.iter().map(|b| b.id).ne(inner
                .layout
                .display
                .iter()
                .copied());
            Ok(patch)
        });
        if matches!(result, Err(Error::WrongDocument)) {
            // The other document's changes are merged in now; the page can't be trusted.
            self.guard.poison();
        }
        result
    }

    /// The whole page, for saving or for a new device.
    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        self.guard
            .run(|| Ok(self.inner.doc.export(ExportMode::Snapshot)?))
    }

    /// The changes made since `since`, for appending to the update log or syncing.
    pub fn export_updates(&self, since: &VersionVector) -> Result<Vec<u8>> {
        self.guard
            .run(|| Ok(self.inner.doc.export(ExportMode::updates(since))?))
    }

    /// Imported updates still waiting for changes from another writer, which neither the
    /// snapshot nor [`Self::export_updates`] holds. Saved beside a snapshot, they apply once
    /// what they wait for arrives.
    pub fn export_pending(&self) -> Result<Option<Vec<u8>>> {
        self.guard
            .run(|| Ok(self.inner.doc.export_pending_updates()?))
    }

    /// The version this page is at: pass it to [`Page::export_updates`] later.
    pub fn version(&self) -> VersionVector {
        self.inner.doc.oplog_vv()
    }

    /// Whether the layout kept up incrementally matches one computed from scratch. For tests:
    /// the fuzz tests call it after every edit.
    #[doc(hidden)]
    pub fn layout_is_consistent(&self) -> bool {
        let fresh = Layout::compute(&self.inner.doc, Some(self.inner.genesis));
        let have = &self.inner.layout;
        fresh.body == have.body
            && fresh.marker_pos == have.marker_pos
            && fresh.body_len == have.body_len
            && fresh.display == have.display
            && fresh.indent == have.indent
            && fresh.anchors == have.anchors
    }

    /// Runs `f` inside the panic guard. Lets tests check that a panic poisons the page.
    #[cfg(test)]
    pub(crate) fn run_guarded<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.guard.run(f)
    }
}

impl Inner {
    fn body(&self) -> LoroText {
        self.doc.get_text("body")
    }

    fn order(&self) -> loro::LoroMovableList {
        self.doc.get_movable_list("order")
    }

    fn check(&self, id: BlockId) -> Result<()> {
        if self.layout.body_at(id).is_some() {
            Ok(())
        } else {
            Err(Error::UnknownBlock(id))
        }
    }

    fn attrs_map(&self, id: BlockId) -> Result<LoroMap> {
        let blocks = self.doc.get_map("blocks");
        match blocks.get(&id.to_string()) {
            Some(ValueOrContainer::Container(c)) => c
                .into_map()
                .map_err(|_| Error::Loro("block attributes aren't a map".into())),
            // A block whose attributes were deleted concurrently: give it fresh ones.
            _ => Ok(blocks.insert_container(&id.to_string(), LoroMap::new())?),
        }
    }

    fn attrs_map_opt(&self, id: BlockId) -> Option<LoroMap> {
        self.doc
            .get_map("blocks")
            .get(&id.to_string())?
            .into_container()
            .ok()?
            .into_map()
            .ok()
    }

    fn set_attr(&self, id: BlockId, key: &str, value: impl Into<LoroValue>) -> Result<()> {
        self.attrs_map(id)?.insert(key, value)?;
        Ok(())
    }

    fn is_hidden(&self, id: BlockId) -> bool {
        self.attrs_map_opt(id)
            .is_some_and(|m| attrs::get_bool(&m, "hidden"))
    }

    fn shown_indent(&self, id: BlockId) -> u8 {
        self.layout
            .display_at(id)
            .map_or(0, |i| self.layout.indent[i])
    }

    /// The block's text as stored, stray markers included.
    fn raw_text(&self, id: BlockId) -> Result<String> {
        let (start, end) = self.layout.text_range(id).ok_or(Error::UnknownBlock(id))?;
        Ok(self.body().slice(start, end)?)
    }

    fn block(&self, id: BlockId) -> Result<Block> {
        let (start, end) = self.layout.text_range(id).ok_or(Error::UnknownBlock(id))?;
        let mut spans: Vec<Span> = Vec::new();
        if end > start {
            for segment in self.body().slice_delta(start, end, PosType::Unicode)? {
                let TextDelta::Insert { insert, attributes } = segment else {
                    continue;
                };
                let text: String = insert.chars().filter(|&c| c != MARKER).collect();
                if text.is_empty() {
                    continue;
                }
                let marks = attrs::marks_from(attributes.as_ref());
                match spans.last_mut() {
                    Some(last) if last.marks == marks => last.text.push_str(&text),
                    _ => spans.push(Span { text, marks }),
                }
            }
        }
        let map = self.attrs_map_opt(id);
        Ok(Block {
            id,
            kind: attrs::read_kind(map.as_ref()),
            indent: self.shown_indent(id),
            spans,
        })
    }

    /// Converts a UTF-16 position in block `id`'s text into a position in `body`, checking it.
    fn body_pos(&self, id: BlockId, pos: usize) -> Result<usize> {
        let (start, _) = self.layout.text_range(id).ok_or(Error::UnknownBlock(id))?;
        let text = self.raw_text(id)?;
        let mut units = 0;
        for (i, ch) in text.chars().enumerate() {
            if units == pos {
                return Ok(start + i);
            }
            if ch != MARKER {
                units += ch.len_utf16();
            }
            if units > pos {
                return Err(Error::InsideCharacter(pos));
            }
        }
        if units == pos {
            Ok(start + text.chars().count())
        } else {
            Err(Error::OutOfRange { pos, len: units })
        }
    }

    /// Inserts a new block's marker at body position `at`, inside block `owner`'s text, with
    /// its attributes. The new block takes the owner's indent.
    /// Adds a block marker at body position `at`, inside `owner`'s text range. `worked_in` says
    /// whether this is an edit in or next to `owner` itself (a split, or a block added after it),
    /// which brings back a deleted first block, as typing into it does.
    fn insert_marker(
        &mut self,
        owner: BlockId,
        at: usize,
        kind: &BlockKind,
        worked_in: bool,
    ) -> Result<BlockId> {
        if worked_in && owner == self.genesis && self.is_hidden(owner) {
            self.set_attr(owner, "hidden", false)?;
        }
        let indent = self.shown_indent(owner);
        let id = BlockId::new();
        let body = self.body();
        body.insert(at, &MARKER.to_string())?;
        body.mark(at..at + 1, BLOCK_STYLE, id.to_string())?;
        // A marker inserted inside formatted text picks up its formatting. Take it off, so text
        // typed at the start of the new block never inherits formatting through the marker.
        if let Some(TextDelta::Insert {
            attributes: Some(found),
            ..
        }) = body
            .slice_delta(at, at + 1, PosType::Unicode)?
            .into_iter()
            .next()
        {
            for key in found.keys().filter(|k| k.as_str() != BLOCK_STYLE) {
                body.unmark(at..at + 1, key)?;
            }
        }
        let map = self
            .doc
            .get_map("blocks")
            .insert_container(&id.to_string(), LoroMap::new())?;
        attrs::write_kind(&map, kind)?;
        if indent > 0 {
            map.insert("indent", i64::from(indent))?;
        }
        self.layout.insert_after(owner, id, at, indent);
        Ok(id)
    }

    fn order_index(&self, id: BlockId) -> Option<usize> {
        let id = id.to_string();
        self.order()
            .to_vec()
            .iter()
            .position(|v| matches!(v, LoroValue::String(s) if **s == *id))
    }

    /// Makes the block after `id` in the body head its own group, at the place it is shown now,
    /// so it stays put when `id` moves or goes.
    fn pin_next(&mut self, id: BlockId) -> Result<()> {
        let Some(next) = self.layout.body_next(id) else {
            return Ok(());
        };
        if self.layout.anchored.contains(&next) {
            return Ok(());
        }
        let head = self.layout.head(id).ok_or(Error::UnknownBlock(id))?;
        let at = self.order_index(head).map_or(0, |i| i + 1);
        self.order().insert(at, next.to_string())?;
        self.layout = Layout::compute(&self.doc, Some(self.genesis));
        Ok(())
    }

    /// Takes `id` out of `order` and drops its attributes.
    fn forget(&mut self, id: BlockId) -> Result<()> {
        while let Some(i) = self.order_index(id) {
            self.order().delete(i, 1)?;
        }
        self.doc.get_map("blocks").delete(&id.to_string())?;
        Ok(())
    }

    fn delete_block(&mut self, id: BlockId) -> Result<()> {
        self.check(id)?;
        let (start, end) = self.layout.text_range(id).ok_or(Error::UnknownBlock(id))?;
        if id == self.genesis {
            // The first block's marker stays, so text typed into it elsewhere has a home.
            self.body().delete(start, end - start)?;
            self.set_attr(id, "hidden", true)?;
        } else {
            if self.layout.anchored.contains(&id) {
                self.pin_next(id)?;
            }
            self.body().delete(start - 1, end - start + 1)?;
            self.forget(id)?;
        }
        self.layout = Layout::compute(&self.doc, Some(self.genesis));
        Ok(())
    }

    /// Commits, recomputes the layout and reports what changed against `before`, the layout as
    /// it was before the edit started.
    fn finish(&mut self, before: Layout, touched: Vec<BlockId>) -> Result<Patch> {
        self.doc.commit();
        self.layout = Layout::compute(&self.doc, Some(self.genesis));
        let after = &self.layout;
        let mut updated: Vec<BlockId> = touched
            .into_iter()
            .filter(|&id| after.display_at(id).is_some())
            .collect();
        for (i, &id) in after.display.iter().enumerate() {
            let was = before.display_at(id).map(|j| before.indent[j]);
            if was != Some(after.indent[i]) && !updated.contains(&id) {
                updated.push(id);
            }
        }
        Ok(Patch {
            updated,
            removed: before
                .display
                .iter()
                .copied()
                .filter(|&id| after.display_at(id).is_none())
                .collect(),
            order_changed: before.display != after.display,
        })
    }
}

/// Every inline formatting key.
const INLINE_KEYS: [&str; 6] = [
    "bold",
    "italic",
    "underline",
    "strikethrough",
    "code",
    "link",
];

fn stored_indent(doc: &LoroDoc, id: BlockId) -> u8 {
    let map = doc
        .get_map("blocks")
        .get(&id.to_string())
        .and_then(|v| v.into_container().ok())
        .and_then(|c| c.into_map().ok());
    attrs::read_indent(map.as_ref())
}

/// Text written to a page can't contain the block marker.
fn sanitize(text: &str) -> String {
    text.replace(MARKER, "\u{FFFD}")
}

fn count_isize(text: &str) -> isize {
    isize::try_from(text.chars().count()).unwrap_or(isize::MAX)
}

fn utf16_len(raw: &str) -> usize {
    raw.chars()
        .filter(|&c| c != MARKER)
        .map(char::len_utf16)
        .sum()
}

fn marks_of(marks: &drusite_model::Marks) -> Vec<Mark> {
    let mut out = Vec::new();
    if marks.bold {
        out.push(Mark::Bold);
    }
    if marks.italic {
        out.push(Mark::Italic);
    }
    if marks.underline {
        out.push(Mark::Underline);
    }
    if marks.strikethrough {
        out.push(Mark::Strikethrough);
    }
    if marks.code {
        out.push(Mark::Code);
    }
    if let Some(href) = &marks.link {
        out.push(Mark::Link { href: href.clone() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loro_panic_poisons_the_page_until_it_is_reloaded() {
        let mut page = Page::create().unwrap();
        let first = page.order()[0];
        page.insert_text(first, 0, "kept").unwrap();
        let saved = page.export_snapshot().unwrap();

        let result: Result<()> = page.run_guarded(|| panic!("simulated Loro panic"));
        assert!(matches!(result, Err(Error::Poisoned)));
        assert!(page.is_poisoned());
        assert!(matches!(
            page.insert_text(first, 0, "x"),
            Err(Error::Poisoned)
        ));
        assert!(matches!(page.blocks(), Err(Error::Poisoned)));

        let reloaded = Page::load(&saved).unwrap();
        assert_eq!(reloaded.block(first).unwrap().text(), "kept");
    }
}
