// SPDX-License-Identifier: AGPL-3.0-or-later

//! Working out a page's blocks and their display order from its document.
//!
//! The layout is a pure function of the document's state, so every device that has the same
//! changes shows the same page, whatever order the changes arrived in.

mod seq;

use std::collections::{HashMap, HashSet};

use drusite_model::BlockId;
use loro::{LoroDoc, LoroValue, TextDelta};

use crate::MARKER;
use crate::attrs::{self, BLOCK_STYLE};
use seq::{Item, Seq};

/// A block's marker in the body, and how far the block reaches: the marker itself and the
/// block's text up to the next marker. Storing the span rather than the marker's position means
/// typing changes one block, not the position of every block after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Marker {
    id: BlockId,
    span: usize,
}

impl Item for Marker {
    fn id(&self) -> BlockId {
        self.id
    }
    fn weight(&self) -> usize {
        self.span
    }
}

/// A block on screen and the indent it is shown at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shown {
    id: BlockId,
    indent: u8,
}

impl Item for Shown {
    fn id(&self) -> BlockId {
        self.id
    }
    fn weight(&self) -> usize {
        0
    }
}

/// Where every block is, in the body and on screen.
///
/// Both lists are [`Seq`]s, so Enter and Backspace cost about the square root of the page's
/// length rather than the length.
#[derive(Debug, Default, Clone)]
pub(crate) struct Layout {
    /// Blocks in `body` order.
    body: Seq<Marker>,
    /// Characters before the first marker (normally none).
    lead: usize,
    /// Blocks listed in `order`, in display order.
    pub(crate) anchors: Vec<BlockId>,
    pub(crate) anchored: HashSet<BlockId>,
    /// Blocks in display order, hidden ones left out.
    display: Seq<Shown>,
}

impl Layout {
    pub(crate) fn compute(doc: &LoroDoc, genesis: Option<BlockId>) -> Self {
        let mut layout = Self::default();

        // Markers, in body order. A marker whose ID is missing, unreadable or a repeat is left
        // as an invisible character inside the block before it; nothing is dropped.
        let mut body: Vec<BlockId> = Vec::new();
        let mut marker_pos: Vec<usize> = Vec::new();
        let mut seen: HashSet<BlockId> = HashSet::new();
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
                    && seen.insert(id)
                {
                    body.push(id);
                    marker_pos.push(pos);
                }
                pos += 1;
            }
        }
        let body_len = pos;
        layout.lead = marker_pos.first().copied().unwrap_or(body_len);
        let markers = body
            .iter()
            .enumerate()
            .map(|(i, &id)| Marker {
                id,
                span: marker_pos.get(i + 1).copied().unwrap_or(body_len) - marker_pos[i],
            })
            .collect();
        layout.body = Seq::from_vec(markers);

        // Anchors: the `order` list, first occurrence of each live block.
        for value in doc.get_movable_list("order").to_vec() {
            if let LoroValue::String(s) = value
                && let Ok(id) = s.parse::<BlockId>()
                && seen.contains(&id)
                && layout.anchored.insert(id)
            {
                layout.anchors.push(id);
            }
        }
        // The first block in the body always heads a group, so every block has one.
        if let Some(&first) = body.first()
            && layout.anchored.insert(first)
        {
            layout.anchors.insert(0, first);
        }

        // Each unanchored block follows the nearest anchored block before it in the body.
        let mut members: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        let mut head = None;
        for &id in &body {
            if layout.anchored.contains(&id) {
                head = Some(id);
            } else if let Some(h) = head {
                members.entry(h).or_default().push(id);
            }
        }
        let mut order = Vec::with_capacity(body.len());
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
        let mut shown = Vec::with_capacity(order.len());
        for id in order {
            let stored = attrs::read_indent(attr(id).as_ref());
            let indent = prev.map_or(0, |p| stored.min(p.saturating_add(1)));
            shown.push(Shown { id, indent });
            prev = Some(indent);
        }
        layout.display = Seq::from_vec(shown);
        layout
    }

    /// Whether `other` lays the page out exactly as this does.
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        self.lead == other.lead
            && self.body.iter().eq(other.body.iter())
            && self.display.iter().eq(other.display.iter())
            && self.anchors == other.anchors
    }

    /// The length of `body` in Unicode scalar values.
    pub(crate) fn body_len(&self) -> usize {
        self.lead + self.body.weight()
    }

    /// The last block in the body.
    pub(crate) fn body_last(&self) -> Option<BlockId> {
        self.body.last().map(|m| m.id)
    }

    /// Where block `id`'s marker is in the body.
    pub(crate) fn marker_pos(&self, id: BlockId) -> Option<usize> {
        self.body.find(id).map(|(_, before, _)| self.lead + before)
    }

    /// The blocks in display order.
    pub(crate) fn display_ids(&self) -> Vec<BlockId> {
        self.display_iter().collect()
    }

    pub(crate) fn display_iter(&self) -> impl Iterator<Item = BlockId> + '_ {
        self.display.iter().map(|s| s.id)
    }

    /// Every displayed block with the indent it is shown at, in display order.
    pub(crate) fn shown(&self) -> impl Iterator<Item = (BlockId, u8)> + '_ {
        self.display.iter().map(|s| (s.id, s.indent))
    }

    /// The indent block `id` is shown at, if it is shown.
    pub(crate) fn indent_of(&self, id: BlockId) -> Option<u8> {
        self.display.find(id).map(|(_, _, s)| s.indent)
    }

    pub(crate) fn body_at(&self, id: BlockId) -> Option<usize> {
        self.body.index_of(id)
    }

    pub(crate) fn display_at(&self, id: BlockId) -> Option<usize> {
        self.display.index_of(id)
    }

    /// The body range of a block's text (after its marker, up to the next marker).
    pub(crate) fn text_range(&self, id: BlockId) -> Option<(usize, usize)> {
        let (_, before, m) = self.body.find(id)?;
        let at = self.lead + before;
        Some((at + 1, at + m.span))
    }

    pub(crate) fn text_len(&self, id: BlockId) -> usize {
        self.text_range(id).map_or(0, |(s, e)| e - s)
    }

    /// Records `n` characters inserted (positive) or deleted (negative) inside block `id`'s text,
    /// so a keystroke doesn't need a full recompute.
    pub(crate) fn shift_after(&mut self, id: BlockId, delta: isize) {
        let Some(i) = self.body_at(id) else { return };
        self.body
            .update(i, |m| m.span = m.span.saturating_add_signed(delta));
    }

    /// The block after `id` in the body, if any.
    pub(crate) fn body_next(&self, id: BlockId) -> Option<BlockId> {
        let i = self.body_at(id)?;
        self.body.get(i + 1).map(|m| m.id)
    }

    pub(crate) fn body_prev(&self, id: BlockId) -> Option<BlockId> {
        let i = self.body_at(id)?;
        self.body.get(i.checked_sub(1)?).map(|m| m.id)
    }

    pub(crate) fn display_prev(&self, id: BlockId) -> Option<BlockId> {
        let i = self.display_at(id)?;
        self.display.get(i.checked_sub(1)?).map(|s| s.id)
    }

    /// The anchored block whose group `id` belongs to (itself, if it is anchored).
    pub(crate) fn head(&self, id: BlockId) -> Option<BlockId> {
        let i = self.body_at(id)?;
        self.body
            .iter_back_from(i)
            .map(|m| m.id)
            .find(|b| self.anchored.contains(b))
    }
}

/// Incremental updates for the structural edits that happen while typing (Enter, Backspace), so
/// they don't recompute the whole page. Each leaves the layout exactly as [`Layout::compute`]
/// would.
impl Layout {
    /// A new, unanchored block `new` whose marker was just inserted at body position `at`, inside
    /// block `owner`'s text, shown at `indent` (the owner's own shown indent).
    pub(crate) fn insert_after(&mut self, owner: BlockId, new: BlockId, at: usize, indent: u8) {
        let Some((i, before, m)) = self.body.find(owner) else {
            return;
        };
        // The owner's text is cut at `at`: what is before stays the owner's, the rest and the
        // new marker are the new block's.
        let owner_at = self.lead + before;
        debug_assert!(
            owner_at < at && at <= owner_at + m.span,
            "marker outside its owner"
        );
        let kept = at.saturating_sub(owner_at);
        self.body.update(i, |o| o.span = kept);
        self.body.insert(
            i + 1,
            Marker {
                id: new,
                span: m.span + 1 - kept,
            },
        );
        // Unanchored and right after its owner in the body: in the owner's group, right after it.
        let Some(d) = self.display_at(owner) else {
            return;
        };
        self.display.insert(d + 1, Shown { id: new, indent });
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
        // Its text, less the deleted marker, now belongs to the block before it.
        let gone = self.body.remove(i);
        let text = gone.span - 1;
        match i.checked_sub(1) {
            Some(p) => self.body.update(p, |m| m.span += text),
            None => self.lead += text,
        }
        if self.anchored.remove(&id) {
            self.anchors.retain(|&a| a != id);
        }
        if let Some(d) = self.display_at(id) {
            self.display.remove(d);
            // The blocks after it now follow a different block; re-check their indents.
            let mut prev = d
                .checked_sub(1)
                .and_then(|p| self.display.get(p))
                .map(|s| s.indent);
            self.display.update_from(d, |s| {
                let shown = prev.map_or(0, |p| stored_indent(s.id).min(p.saturating_add(1)));
                if shown == s.indent {
                    return false;
                }
                s.indent = shown;
                reindented.push(s.id);
                prev = Some(shown);
                true
            });
        }
        reindented
    }
}
