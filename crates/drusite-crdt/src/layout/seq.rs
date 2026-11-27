// SPDX-License-Identifier: AGPL-3.0-or-later

//! A list of blocks that inserts, removes and finds a block in time that grows with the square
//! root of its length, not the length itself.
//!
//! A plain `Vec` shifts everything after the edit on every Enter and Backspace. That is cheap per
//! byte, but it is still work in proportion to the page, and some CPUs pay far more per byte
//! than others: glibc copies an overlapping shift of a few bytes with `rep movsb`, which Intel
//! CPUs without fast short `rep movsb` (FSRM) run several times slower. Here the list is cut into
//! chunks of about √n items, so an edit touches one chunk and a lookup sums one number per chunk.
//!
//! Each item carries a weight (a block's length in the body, say) and the list keeps the sum per
//! chunk, so "how much weight is before this item" costs the same as finding it.

use std::collections::HashMap;

use drusite_model::BlockId;

/// What the list holds.
pub(crate) trait Item: Copy {
    fn id(&self) -> BlockId;
    fn weight(&self) -> usize;
}

/// The smallest chunk size; below this, chunk bookkeeping costs more than shifting. Tiny in unit
/// tests, so they split, merge and re-cut chunks all the time.
const MIN_CAP: usize = if cfg!(test) { 4 } else { 64 };

#[derive(Debug, Clone)]
struct Chunk<T> {
    /// This chunk's stable name, which `slot_of` refers to.
    slot: u32,
    items: Vec<T>,
    weight: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Seq<T> {
    chunks: Vec<Chunk<T>>,
    /// Which chunk (by slot) holds each item.
    slot_of: HashMap<BlockId, u32>,
    /// Where each slot's chunk is in `chunks`; `u32::MAX` for a free slot.
    slot_pos: Vec<u32>,
    free: Vec<u32>,
    len: usize,
    weight: usize,
    /// The target chunk size. A chunk splits at twice this.
    cap: usize,
}

impl<T> Default for Seq<T> {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            slot_of: HashMap::new(),
            slot_pos: Vec::new(),
            free: Vec::new(),
            len: 0,
            weight: 0,
            cap: MIN_CAP,
        }
    }
}

impl<T: Item> Seq<T> {
    /// A list of `items`, in order. Repeated IDs are a caller's error.
    pub(crate) fn from_vec(items: Vec<T>) -> Self {
        let mut seq = Self {
            cap: cap_for(items.len()),
            ..Self::default()
        };
        seq.len = items.len();
        let mut rest = items.as_slice();
        while !rest.is_empty() {
            let (head, tail) = rest.split_at(rest.len().min(seq.cap));
            rest = tail;
            let slot = seq.new_slot(seq.chunks.len());
            for item in head {
                seq.slot_of.insert(item.id(), slot);
            }
            let weight = head.iter().map(Item::weight).sum();
            seq.weight += weight;
            seq.chunks.push(Chunk {
                slot,
                items: head.to_vec(),
                weight,
            });
        }
        seq
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The sum of every item's weight.
    pub(crate) fn weight(&self) -> usize {
        self.weight
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + '_ {
        self.chunks.iter().flat_map(|c| c.items.iter())
    }

    pub(crate) fn last(&self) -> Option<T> {
        self.chunks.last().and_then(|c| c.items.last().copied())
    }

    /// Where `id` is: its index, the weight of everything before it, and the item.
    pub(crate) fn find(&self, id: BlockId) -> Option<(usize, usize, T)> {
        let slot = *self.slot_of.get(&id)?;
        let c = self.slot_pos[slot as usize] as usize;
        let (mut index, mut weight) = self.chunks[..c]
            .iter()
            .fold((0, 0), |(n, w), ch| (n + ch.items.len(), w + ch.weight));
        for item in &self.chunks[c].items {
            if item.id() == id {
                return Some((index, weight, *item));
            }
            index += 1;
            weight += item.weight();
        }
        unreachable!("slot_of names a chunk that doesn't hold the item")
    }

    pub(crate) fn index_of(&self, id: BlockId) -> Option<usize> {
        self.find(id).map(|(i, _, _)| i)
    }

    pub(crate) fn get(&self, index: usize) -> Option<T> {
        let (c, off) = self.locate(index)?;
        Some(self.chunks[c].items[off])
    }

    /// Items before `index` and the one at it, nearest first.
    pub(crate) fn iter_back_from(&self, index: usize) -> impl Iterator<Item = &T> + '_ {
        let (c, off) = self.locate(index).unwrap_or((self.chunks.len(), 0));
        self.chunks
            .get(c)
            .into_iter()
            .flat_map(move |ch| ch.items[..=off].iter().rev())
            .chain(
                self.chunks[..c.min(self.chunks.len())]
                    .iter()
                    .rev()
                    .flat_map(|ch| ch.items.iter().rev()),
            )
    }

    /// Calls `f` on each item from `index` on, in order, until it returns false.
    pub(crate) fn update_from(&mut self, index: usize, mut f: impl FnMut(&mut T) -> bool) {
        let Some((c, mut off)) = self.locate(index) else {
            return;
        };
        let mut total = self.weight;
        for ch in &mut self.chunks[c..] {
            let mut go = true;
            for item in &mut ch.items[off..] {
                ch.weight -= item.weight();
                total -= item.weight();
                go = f(item);
                ch.weight += item.weight();
                total += item.weight();
                if !go {
                    break;
                }
            }
            if !go {
                break;
            }
            off = 0;
        }
        self.weight = total;
    }

    /// Changes the item at `index` (not its ID).
    pub(crate) fn update(&mut self, index: usize, f: impl FnOnce(&mut T)) {
        let Some((c, off)) = self.locate(index) else {
            return;
        };
        let ch = &mut self.chunks[c];
        let item = &mut ch.items[off];
        let id = item.id();
        ch.weight -= item.weight();
        self.weight -= item.weight();
        f(item);
        debug_assert_eq!(item.id(), id, "update changed an item's ID");
        ch.weight += item.weight();
        self.weight += item.weight();
    }

    /// Inserts `item` at `index` (at most [`Self::len`]).
    pub(crate) fn insert(&mut self, index: usize, item: T) {
        assert!(index <= self.len, "insert past the end");
        if self.chunks.is_empty() {
            let slot = self.new_slot(0);
            self.chunks.push(Chunk {
                slot,
                items: Vec::with_capacity(2 * self.cap),
                weight: 0,
            });
        }
        // At the end of a chunk rather than the start of the next, so appending stays in the
        // last chunk.
        let (c, off) = if index == self.len {
            let c = self.chunks.len() - 1;
            (c, self.chunks[c].items.len())
        } else {
            self.locate(index).expect("index is in range")
        };
        let ch = &mut self.chunks[c];
        ch.items.insert(off, item);
        ch.weight += item.weight();
        self.slot_of.insert(item.id(), ch.slot);
        self.len += 1;
        self.weight += item.weight();
        if self.chunks[c].items.len() >= 2 * self.cap {
            self.split(c);
        }
    }

    /// Removes and returns the item at `index`.
    pub(crate) fn remove(&mut self, index: usize) -> T {
        let (c, off) = self.locate(index).expect("remove past the end");
        let ch = &mut self.chunks[c];
        let item = ch.items.remove(off);
        ch.weight -= item.weight();
        self.slot_of.remove(&item.id());
        self.len -= 1;
        self.weight -= item.weight();
        if self.chunks[c].items.is_empty() {
            self.drop_chunk(c);
        } else if self.chunks[c].items.len() < self.cap / 4 {
            self.merge_small(c);
        }
        item
    }

    /// The chunk and the offset in it of `index`.
    fn locate(&self, mut index: usize) -> Option<(usize, usize)> {
        for (c, ch) in self.chunks.iter().enumerate() {
            if index < ch.items.len() {
                return Some((c, index));
            }
            index -= ch.items.len();
        }
        None
    }

    fn new_slot(&mut self, pos: usize) -> u32 {
        let pos = u32::try_from(pos).expect("fewer than 2^32 chunks");
        if let Some(slot) = self.free.pop() {
            self.slot_pos[slot as usize] = pos;
            slot
        } else {
            self.slot_pos.push(pos);
            u32::try_from(self.slot_pos.len() - 1).expect("fewer than 2^32 chunks")
        }
    }

    /// Chunks from `from` on have moved in `chunks`; tells their slots.
    fn renumber_from(&mut self, from: usize) {
        for (c, ch) in self.chunks.iter().enumerate().skip(from) {
            self.slot_pos[ch.slot as usize] = c as u32;
        }
    }

    fn split(&mut self, c: usize) {
        if self.chunks.len() > 2 * self.cap {
            // The page has grown well past the size the chunks were cut for: cut them again.
            self.rebuild();
            return;
        }
        let half = self.chunks[c].items.len() / 2;
        let tail: Vec<T> = self.chunks[c].items.split_off(half);
        let tail_weight: usize = tail.iter().map(Item::weight).sum();
        self.chunks[c].weight -= tail_weight;
        let slot = self.new_slot(c + 1);
        for item in &tail {
            self.slot_of.insert(item.id(), slot);
        }
        self.chunks.insert(
            c + 1,
            Chunk {
                slot,
                items: tail,
                weight: tail_weight,
            },
        );
        self.renumber_from(c + 2);
    }

    fn drop_chunk(&mut self, c: usize) {
        let ch = self.chunks.remove(c);
        self.slot_pos[ch.slot as usize] = u32::MAX;
        self.free.push(ch.slot);
        self.renumber_from(c);
    }

    /// Folds a nearly empty chunk into a neighbour that has room, so chunks stay few.
    fn merge_small(&mut self, c: usize) {
        let n = self.chunks[c].items.len();
        let fits = |i: usize| self.chunks[i].items.len() + n < 2 * self.cap;
        let into = if c > 0 && fits(c - 1) {
            c - 1
        } else if c + 1 < self.chunks.len() && fits(c + 1) {
            c + 1
        } else {
            return;
        };
        let moved = std::mem::take(&mut self.chunks[c].items);
        let weight = self.chunks[c].weight;
        let slot = self.chunks[into].slot;
        for item in &moved {
            self.slot_of.insert(item.id(), slot);
        }
        let target = &mut self.chunks[into];
        target.weight += weight;
        if into < c {
            target.items.extend(moved);
        } else {
            target.items.splice(0..0, moved);
        }
        self.drop_chunk(c);
    }

    fn rebuild(&mut self) {
        let items: Vec<T> = self.iter().copied().collect();
        *self = Self::from_vec(items);
    }

    /// Checks every invariant. For tests.
    #[cfg(test)]
    pub(crate) fn check(&self) {
        let mut len = 0;
        let mut weight = 0;
        for (c, ch) in self.chunks.iter().enumerate() {
            assert!(!ch.items.is_empty(), "empty chunk {c}");
            assert_eq!(
                self.slot_pos[ch.slot as usize] as usize, c,
                "slot_pos of chunk {c}"
            );
            assert_eq!(ch.weight, ch.items.iter().map(Item::weight).sum::<usize>());
            for item in &ch.items {
                assert_eq!(self.slot_of.get(&item.id()), Some(&ch.slot));
            }
            len += ch.items.len();
            weight += ch.weight;
        }
        assert_eq!(len, self.len);
        assert_eq!(weight, self.weight);
        assert_eq!(self.slot_of.len(), self.len);
    }
}

/// About √n, so a lookup's two parts (one sum per chunk, one scan of a chunk) cost the same.
fn cap_for(len: usize) -> usize {
    len.isqrt().max(MIN_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct It(BlockId, usize);

    impl Item for It {
        fn id(&self) -> BlockId {
            self.0
        }
        fn weight(&self) -> usize {
            self.1
        }
    }

    /// Random inserts, removes and updates against a plain `Vec`, across chunk splits, merges and
    /// rebuilds.
    #[test]
    fn matches_a_vec() {
        for seed in 0..20 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let start: Vec<It> = (0..rng.usize(0..300))
                .map(|_| It(BlockId::new(), rng.usize(1..10)))
                .collect();
            let mut want = start.clone();
            let mut seq = Seq::from_vec(start);
            for _ in 0..3_000 {
                // Lean towards growing, so the list outgrows its first chunk size.
                match rng.u8(0..10) {
                    0..=4 => {
                        let i = rng.usize(..=want.len());
                        let it = It(BlockId::new(), rng.usize(1..10));
                        want.insert(i, it);
                        seq.insert(i, it);
                    }
                    5..=7 if !want.is_empty() => {
                        let i = rng.usize(..want.len());
                        assert_eq!(seq.remove(i), want.remove(i));
                    }
                    8 if !want.is_empty() => {
                        let i = rng.usize(..want.len());
                        let w = rng.usize(1..10);
                        want[i].1 = w;
                        seq.update(i, |it| it.1 = w);
                    }
                    _ if !want.is_empty() => {
                        let i = rng.usize(..want.len());
                        for it in want[i..].iter_mut().take(3) {
                            it.1 += 1;
                        }
                        let mut n = 0;
                        seq.update_from(i, |it| {
                            it.1 += 1;
                            n += 1;
                            n < 3
                        });
                    }
                    _ => {}
                }
                seq.check();
                assert_eq!(seq.len(), want.len());
                assert_eq!(seq.weight(), want.iter().map(|i| i.1).sum::<usize>());
                if !want.is_empty() {
                    let i = rng.usize(..want.len());
                    let before: usize = want[..i].iter().map(|i| i.1).sum();
                    assert_eq!(seq.find(want[i].0), Some((i, before, want[i])));
                    assert_eq!(seq.get(i), Some(want[i]));
                    let back: Vec<It> = seq.iter_back_from(i).copied().collect();
                    let mut w: Vec<It> = want[..=i].to_vec();
                    w.reverse();
                    assert_eq!(back, w);
                }
            }
            assert_eq!(seq.iter().copied().collect::<Vec<_>>(), want);
        }
    }
}
