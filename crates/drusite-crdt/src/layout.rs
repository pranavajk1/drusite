// SPDX-License-Identifier: AGPL-3.0-or-later

//! Working out a page's blocks and their display order from its document.
//!
//! The layout is a pure function of the document's state, so every device that has the same
//! changes shows the same page, whatever order the changes arrived in.

use std::collections::{HashMap, HashSet};

use drusite_model::BlockId;
use loro::{LoroDoc, LoroValue, TextDelta};

use crate::MARKER;
use crate::attrs::{self, BLOCK_STYLE};

/// Where every block is, in the body and on screen.
#[derive(Debug, Default, Clone)]
pub(crate) struct Layout {
    /// Blocks in `body` order.
    pub(crate) body: Vec<BlockId>,
    /// The position (in Unicode scalar values) of each block's marker, parallel to `body`.
    pub(crate) marker_pos: Vec<usize>,
    pub(crate) body_index: PosIndex,
    /// The length of `body` in Unicode scalar values.
    pub(crate) body_len: usize,
    /// Blocks listed in `order`, in display order.
    pub(crate) anchors: Vec<BlockId>,
    pub(crate) anchored: HashSet<BlockId>,
    /// Blocks in display order, hidden ones left out.
    pub(crate) display: Vec<BlockId>,
    pub(crate) display_index: PosIndex,
    /// The indent each displayed block is shown at, parallel to `display`.
    pub(crate) indent: Vec<u8>,
}

impl Layout {
    pub(crate) fn compute(doc: &LoroDoc, genesis: Option<BlockId>) -> Self {
        let mut layout = Self::default();

        // Markers, in body order. A marker whose ID is missing, unreadable or a repeat is left
        // as an invisible character inside the block before it; nothing is dropped.
        let mut pos = 0;
        for segment in doc.get_text("body").to_delta() {
            let TextDelta::Insert { insert, attributes } = segment else {
                continue;
            };
            let id = attributes
                .as_ref()
                .and_then(|a| a.get(BLOCK_STYLE))
                .and_then(|v| match v {
                    LoroValue::String(s) => s.parse::<BlockId>().ok(),
                    _ => None,
                });
            for ch in insert.chars() {
                if ch == MARKER
                    && let Some(id) = id
                    && !layout.body_index.contains(&id)
                {
                    layout.body_index.insert_exact(id, layout.body.len());
                    layout.body.push(id);
                    layout.marker_pos.push(pos);
                }
                pos += 1;
            }
        }
        layout.body_len = pos;

        // Anchors: the `order` list, first occurrence of each live block.
        for value in doc.get_movable_list("order").to_vec() {
            if let LoroValue::String(s) = value
                && let Ok(id) = s.parse::<BlockId>()
                && layout.body_index.contains(&id)
                && layout.anchored.insert(id)
            {
                layout.anchors.push(id);
            }
        }
        // The first block in the body always heads a group, so every block has one.
        if let Some(&first) = layout.body.first()
            && layout.anchored.insert(first)
        {
            layout.anchors.insert(0, first);
        }

        // Each unanchored block follows the nearest anchored block before it in the body.
        let mut members: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        let mut head = None;
        for &id in &layout.body {
            if layout.anchored.contains(&id) {
                head = Some(id);
            } else if let Some(h) = head {
                members.entry(h).or_default().push(id);
            }
        }
        let mut order = Vec::with_capacity(layout.body.len());
        for &anchor in &layout.anchors {
            order.push(anchor);
            if let Some(m) = members.get(&anchor) {
                order.extend_from_slice(m);
            }
        }

        // A deleted first block is hidden rather than removed (its marker has to stay, so text
        // typed into it concurrently still has a block), unless it has text or is all there is.
        let blocks = doc.get_map("blocks");
        let attr = |id: BlockId| {
            blocks
                .get(&id.to_string())
                .and_then(|v| v.into_container().ok())
                .and_then(|c| c.into_map().ok())
        };
        if let Some(g) = genesis
            && order.len() > 1
            && attr(g).is_some_and(|m| attrs::get_bool(&m, "hidden"))
            && layout.text_len(g) == 0
        {
            order.retain(|&id| id != g);
        }

        // Indents can't skip a level: each block is at most one deeper than the one before.
        let mut prev: Option<u8> = None;
        for &id in &order {
            let stored = attrs::read_indent(attr(id).as_ref());
            let shown = prev.map_or(0, |p| stored.min(p.saturating_add(1)));
            layout.indent.push(shown);
            prev = Some(shown);
        }
        layout.display_index = PosIndex::build(&order);
        layout.display = order;
        layout
    }

    pub(crate) fn body_at(&self, id: BlockId) -> Option<usize> {
        self.body_index.find(id, &self.body)
    }

    pub(crate) fn display_at(&self, id: BlockId) -> Option<usize> {
        self.display_index.find(id, &self.display)
    }

    /// The body range of a block's text (after its marker, up to the next marker).
    pub(crate) fn text_range(&self, id: BlockId) -> Option<(usize, usize)> {
        let i = self.body_at(id)?;
        let start = self.marker_pos[i] + 1;
        let end = self.marker_pos.get(i + 1).copied().unwrap_or(self.body_len);
        Some((start, end))
    }

    pub(crate) fn text_len(&self, id: BlockId) -> usize {
        self.text_range(id).map_or(0, |(s, e)| e - s)
    }

    /// Records `n` characters inserted (positive) or deleted (negative) inside block `id`'s text,
    /// so a keystroke doesn't need a full recompute.
    pub(crate) fn shift_after(&mut self, id: BlockId, delta: isize) {
        let Some(i) = self.body_at(id) else { return };
        for pos in &mut self.marker_pos[i + 1..] {
            *pos = pos.saturating_add_signed(delta);
        }
        self.body_len = self.body_len.saturating_add_signed(delta);
    }

    /// The block after `id` in the body, if any.
    pub(crate) fn body_next(&self, id: BlockId) -> Option<BlockId> {
        let i = self.body_at(id)?;
        self.body.get(i + 1).copied()
    }

    pub(crate) fn body_prev(&self, id: BlockId) -> Option<BlockId> {
        let i = self.body_at(id)?;
        i.checked_sub(1).map(|p| self.body[p])
    }

    pub(crate) fn display_prev(&self, id: BlockId) -> Option<BlockId> {
        let i = self.display_at(id)?;
        i.checked_sub(1).map(|p| self.display[p])
    }

    /// The anchored block whose group `id` belongs to (itself, if it is anchored).
    pub(crate) fn head(&self, id: BlockId) -> Option<BlockId> {
        let mut i = self.body_at(id)?;
        loop {
            let b = self.body[i];
            if self.anchored.contains(&b) {
                return Some(b);
            }
            i = i.checked_sub(1)?;
        }
    }
}

/// Incremental updates for the structural edits that happen while typing (Enter, Backspace), so
/// they don't recompute the whole page. Each leaves the layout exactly as [`Layout::compute`]
/// would.
impl Layout {
    /// A new, unanchored block `new` whose marker was just inserted at body position `at`, inside
    /// block `owner`'s text, shown at `indent` (the owner's own shown indent).
    pub(crate) fn insert_after(&mut self, owner: BlockId, new: BlockId, at: usize, indent: u8) {
        let Some(i) = self.body_at(owner).map(|i| i + 1) else {
            return;
        };
        for pos in &mut self.marker_pos[i..] {
            *pos += 1;
        }
        self.body.insert(i, new);
        self.marker_pos.insert(i, at);
        self.body_len += 1;
        self.body_index.inserted(new, i, &self.body);
        // Unanchored and right after its owner in the body: in the owner's group, right after it.
        let Some(d) = self.display_at(owner).map(|d| d + 1) else {
            return;
        };
        self.display.insert(d, new);
        self.indent.insert(d, indent);
        self.display_index.inserted(new, d, &self.display);
    }

    /// Block `id`'s marker was just deleted, joining its text onto the block before it in the
    /// body, which is also the block shown before it. `stored_indent` reads a block's indent
    /// attribute, for re-checking the blocks after it.
    /// Returns the blocks whose shown indent changed.
    pub(crate) fn remove_joined(
        &mut self,
        id: BlockId,
        stored_indent: impl Fn(BlockId) -> u8,
    ) -> Vec<BlockId> {
        let mut reindented = Vec::new();
        let Some(i) = self.body_at(id) else {
            return reindented;
        };
        self.body.remove(i);
        self.marker_pos.remove(i);
        for pos in &mut self.marker_pos[i..] {
            *pos -= 1;
        }
        self.body_len -= 1;
        self.body_index.removed(id, &self.body);
        if self.anchored.remove(&id) {
            self.anchors.retain(|&a| a != id);
        }
        if let Some(d) = self.display_at(id) {
            self.display.remove(d);
            self.indent.remove(d);
            self.display_index.removed(id, &self.display);
            // The blocks after it now follow a different block; re-check their indents.
            for j in d..self.display.len() {
                let prev = j.checked_sub(1).map(|p| self.indent[p]);
                let shown = prev.map_or(0, |p| {
                    stored_indent(self.display[j]).min(p.saturating_add(1))
                });
                if shown == self.indent[j] {
                    break;
                }
                self.indent[j] = shown;
                reindented.push(self.display[j]);
            }
        }
        reindented
    }
}

/// Where each block is in a list, kept cheaply while blocks are inserted and removed.
///
/// Rebuilding a map of positions after every Enter costs time in proportion to the page. Instead
/// the map is allowed to go stale by a bounded amount: after `drift` inserts or removals, every
/// stored position is off by at most `drift`, so a lookup checks that many places either side.
/// Past [`MAX_DRIFT`] the map is rebuilt, which keeps both costs small.
#[derive(Debug, Default, Clone)]
pub(crate) struct PosIndex {
    map: HashMap<BlockId, usize>,
    drift: usize,
}

const MAX_DRIFT: usize = 48;

impl PosIndex {
    pub(crate) fn build(list: &[BlockId]) -> Self {
        Self {
            map: list.iter().enumerate().map(|(i, &id)| (id, i)).collect(),
            drift: 0,
        }
    }

    pub(crate) fn insert_exact(&mut self, id: BlockId, at: usize) {
        self.map.insert(id, at);
    }

    pub(crate) fn contains(&self, id: &BlockId) -> bool {
        self.map.contains_key(id)
    }

    pub(crate) fn find(&self, id: BlockId, list: &[BlockId]) -> Option<usize> {
        let guess = *self.map.get(&id)?;
        for d in 0..=self.drift {
            if let Some(i) = guess.checked_add(d)
                && list.get(i) == Some(&id)
            {
                return Some(i);
            }
            if let Some(i) = guess.checked_sub(d)
                && list.get(i) == Some(&id)
            {
                return Some(i);
            }
        }
        None
    }

    /// `id` was just inserted into `list` at `at`.
    pub(crate) fn inserted(&mut self, id: BlockId, at: usize, list: &[BlockId]) {
        self.map.insert(id, at);
        self.shifted(list);
    }

    /// `id` was just removed from `list`.
    pub(crate) fn removed(&mut self, id: BlockId, list: &[BlockId]) {
        self.map.remove(&id);
        self.shifted(list);
    }

    fn shifted(&mut self, list: &[BlockId]) {
        self.drift += 1;
        if self.drift > MAX_DRIFT {
            *self = Self::build(list);
        }
    }
}
