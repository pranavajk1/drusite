// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bringing an edited file back into its page without losing anything.
//!
//! The file was written from the page at some version, its base. What changed in the file is
//! the difference between the base and the file. That difference is made on a copy of the page
//! at the base ([`Page::fork_at`]), as a writer of its own, and the copy's changes are imported
//! into the page as it is now. To the page, the file's edits are a concurrent edit like
//! another device's, so whatever changed in the app since the file was written stays: two edits
//! to the same paragraph both land, and text is never lost.

use std::collections::{HashMap, HashSet};

use drusite_crdt::{Page, Patch, VersionVector};
use drusite_model::{Block, BlockId, BlockKind, Mark, Marks, Span};
use similar::{Algorithm, DiffOp, capture_diff_slices};

use crate::Result;

/// A block as a file wants it. `id` is the block it was, if it names one of the page's.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub id: Option<BlockId>,
    pub kind: BlockKind,
    pub indent: u8,
    pub spans: Vec<Span>,
}

/// Makes `page` what `wanted` says, as an edit made at `base`. Returns what changed in
/// `page`, to show an open editor.
pub fn merge(page: &mut Page, base: &VersionVector, wanted: &[Wanted]) -> Result<Patch> {
    let mut fork = page.fork_at(base)?;
    apply(&mut fork, wanted)?;
    let updates = fork.export_updates(base)?;
    Ok(page.import(&updates)?)
}

/// Turns `page` into `wanted`, changing as little as it can: blocks keep their identity, text
/// changes by the characters that differ, and blocks move only when they must.
pub fn apply(page: &mut Page, wanted: &[Wanted]) -> Result<()> {
    let base: Vec<Block> = page.blocks()?;
    let present: HashSet<BlockId> = base.iter().map(|b| b.id).collect();
    // Each base block is claimed by the first wanted block naming it; a copy-pasted
    // reference later on is a new block.
    let mut claimed = HashSet::new();
    let ids: Vec<Option<BlockId>> = wanted
        .iter()
        .map(|w| {
            w.id.filter(|id| present.contains(id) && claimed.insert(*id))
        })
        .collect();

    // Gone from the file: deleted. A page always keeps one block, so if every block went, the
    // first stays to become the first wanted one.
    let mut keep_first = None;
    if claimed.is_empty()
        && let Some(first) = base.first()
    {
        keep_first = Some(first.id);
    }
    for b in &base {
        if !claimed.contains(&b.id) && Some(b.id) != keep_first {
            page.delete_block(b.id)?;
        }
    }

    // Blocks that keep their order: the longest run of claimed blocks already in file order.
    let position: HashMap<BlockId, usize> = page
        .order()
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    let in_order: Vec<usize> = ids.iter().flatten().map(|id| position[id]).collect();
    let stay: HashSet<usize> = longest_increasing(&in_order).into_iter().collect();

    let mut prev: Option<BlockId> = None;
    let mut placed = Vec::with_capacity(wanted.len());
    for (w, id) in wanted.iter().zip(&ids) {
        let id = match (*id, keep_first.take()) {
            (Some(id), _) => {
                if !stay.contains(&position[&id]) {
                    page.move_block(id, prev)?;
                }
                id
            }
            (None, Some(first)) => {
                // The page's last remaining block, reused for the first wanted one.
                page.move_block(first, prev)?;
                first
            }
            (None, None) => page.insert_block_after(prev, w.kind.clone())?.0,
        };
        let now = page.block(id)?;
        if now.kind != w.kind {
            page.set_kind(id, &w.kind)?;
        }
        set_text(page, id, &now.spans, &w.spans)?;
        placed.push(id);
        prev = Some(id);
    }
    // Indents last, in order: how deep a block may go depends on the block before it.
    for (w, id) in wanted.iter().zip(&placed) {
        if page.block(*id)?.indent != w.indent {
            page.set_indent(*id, w.indent)?;
        }
    }
    Ok(())
}

/// The positions (into `seq`) of a longest strictly increasing subsequence.
fn longest_increasing(seq: &[usize]) -> Vec<usize> {
    // Patience sorting, keeping each pile top's index and a back pointer.
    let mut tops: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; seq.len()];
    for (i, &x) in seq.iter().enumerate() {
        let at = tops.partition_point(|&t| seq[t] < x);
        back[i] = at.checked_sub(1).map(|p| tops[p]);
        if at == tops.len() {
            tops.push(i);
        } else {
            tops[at] = i;
        }
    }
    let mut out = Vec::with_capacity(tops.len());
    let mut at = tops.last().copied();
    while let Some(i) = at {
        out.push(seq[i]);
        at = back[i];
    }
    out.reverse();
    out
}

/// One character's formatting, for comparing.
fn per_char(spans: &[Span]) -> (Vec<char>, Vec<Marks>) {
    let mut chars = Vec::new();
    let mut marks = Vec::new();
    for s in spans {
        for c in s.text.chars() {
            chars.push(c);
            marks.push(s.marks.clone());
        }
    }
    (chars, marks)
}

fn utf16_offsets(chars: &[char]) -> Vec<usize> {
    let mut out = Vec::with_capacity(chars.len() + 1);
    let mut at = 0;
    out.push(0);
    for c in chars {
        at += c.len_utf16();
        out.push(at);
    }
    out
}

/// Reads one on/off mark.
type MarkFlag = fn(&Marks) -> bool;

/// Changes a block's text from `old` to `new`: the characters that differ, then the marks.
fn set_text(page: &mut Page, id: BlockId, old: &[Span], new: &[Span]) -> Result<()> {
    let (old_chars, _) = per_char(old);
    let (new_chars, new_marks) = per_char(new);
    if old_chars != new_chars {
        let at = utf16_offsets(&old_chars);
        let ops = capture_diff_slices(Algorithm::Myers, &old_chars, &new_chars);
        // From the end, so earlier positions still hold.
        for op in ops.iter().rev() {
            let (old_index, old_len, new_index, new_len) = match *op {
                DiffOp::Equal { .. } => continue,
                DiffOp::Delete {
                    old_index, old_len, ..
                } => (old_index, old_len, 0, 0),
                DiffOp::Insert {
                    old_index,
                    new_index,
                    new_len,
                } => (old_index, 0, new_index, new_len),
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => (old_index, old_len, new_index, new_len),
            };
            if old_len > 0 {
                page.delete_text(id, at[old_index], at[old_index + old_len] - at[old_index])?;
            }
            if new_len > 0 {
                let text: String = new_chars[new_index..new_index + new_len].iter().collect();
                page.insert_text(id, at[old_index], &text)?;
            }
        }
    }
    // Marks, compared character by character with what the block now has.
    let (now_chars, now_marks) = per_char(&page.block(id)?.spans);
    if now_chars != new_chars {
        // A kind whose text the page keeps differently (none, for a divider): leave it.
        return Ok(());
    }
    let at = utf16_offsets(&new_chars);
    let flags: [(MarkFlag, Mark); 5] = [
        (|m| m.bold, Mark::Bold),
        (|m| m.italic, Mark::Italic),
        (|m| m.underline, Mark::Underline),
        (|m| m.strikethrough, Mark::Strikethrough),
        (|m| m.code, Mark::Code),
    ];
    for (get, mark) in &flags {
        for (start, end, on) in runs(new_chars.len(), |i| {
            let want = get(&new_marks[i]);
            (want != get(&now_marks[i])).then_some(want)
        }) {
            page.format(id, at[start], at[end], mark, on)?;
        }
    }
    for (start, end, href) in runs(new_chars.len(), |i| {
        (new_marks[i].link != now_marks[i].link).then(|| new_marks[i].link.clone())
    }) {
        match href {
            Some(href) => page.format(id, at[start], at[end], &Mark::Link { href }, true)?,
            None => page.format(
                id,
                at[start],
                at[end],
                &Mark::Link {
                    href: String::new(),
                },
                false,
            )?,
        };
    }
    Ok(())
}

/// Runs of consecutive positions with the same `Some` value: `(start, end, value)`.
fn runs<T: PartialEq>(len: usize, at: impl Fn(usize) -> Option<T>) -> Vec<(usize, usize, T)> {
    let mut out: Vec<(usize, usize, T)> = Vec::new();
    for i in 0..len {
        let Some(v) = at(i) else { continue };
        match out.last_mut() {
            Some((_, end, last)) if *end == i && *last == v => *end = i + 1,
            _ => out.push((i, i + 1, v)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn wanted(page: &Page) -> Vec<Wanted> {
        page.blocks()
            .unwrap()
            .into_iter()
            .map(|b| Wanted {
                id: Some(b.id),
                kind: b.kind,
                indent: b.indent,
                spans: b.spans,
            })
            .collect()
    }

    fn texts(page: &Page) -> Vec<String> {
        page.blocks().unwrap().iter().map(Block::text).collect()
    }

    fn page_of(lines: &[&str]) -> Page {
        let mut page = Page::create().unwrap();
        let w: Vec<Wanted> = lines
            .iter()
            .map(|t| Wanted {
                id: None,
                kind: BlockKind::Paragraph,
                indent: 0,
                spans: vec![Span::plain(*t)],
            })
            .collect();
        apply(&mut page, &w).unwrap();
        page
    }

    #[test]
    fn edits_in_the_file_and_the_app_to_one_paragraph_both_land() {
        let mut page = page_of(&["The quick fox", "Second"]);
        let base = page.version();
        let mut file = wanted(&page);

        // In the app, after the file was written: a word at the end of the first paragraph.
        let first = page.order()[0];
        page.insert_text(first, 13, " jumps").unwrap();
        // In the file: a word in the middle of it, the second paragraph reworded, a new one.
        file[0].spans = vec![Span::plain("The quick brown fox")];
        file[1].spans = vec![Span::plain("Second, reworded")];
        file.push(Wanted {
            id: None,
            kind: BlockKind::Todo { checked: false },
            indent: 0,
            spans: vec![Span::plain("New in the file")],
        });

        merge(&mut page, &base, &file).unwrap();
        assert_eq!(
            texts(&page),
            [
                "The quick brown fox jumps",
                "Second, reworded",
                "New in the file"
            ]
        );
        assert_eq!(page.order()[0], first, "the block kept its identity");
        assert_eq!(
            page.blocks().unwrap()[2].kind,
            BlockKind::Todo { checked: false }
        );
    }

    #[test]
    fn reordering_deleting_and_formatting_in_the_file() {
        let mut page = page_of(&["a", "b", "c", "d"]);
        let base = page.version();
        let ids = page.order();
        let mut file = wanted(&page);
        // d moves to the top, b goes, c becomes a heading at indent 1 with a bold word.
        let d = file.remove(3);
        file.insert(0, d);
        file.remove(2);
        file[2].kind = BlockKind::Heading {
            level: drusite_model::HeadingLevel::H2,
        };
        file[2].indent = 1;
        file[2].spans = vec![
            Span::plain("c "),
            Span {
                text: "bold".into(),
                marks: Marks {
                    bold: true,
                    ..Marks::default()
                },
            },
        ];
        merge(&mut page, &base, &file).unwrap();
        assert_eq!(page.order(), [ids[3], ids[0], ids[2]]);
        let c = &page.blocks().unwrap()[2];
        assert_eq!(c.indent, 1);
        assert_eq!(c.text(), "c bold");
        assert!(c.spans.iter().any(|s| s.text == "bold" && s.marks.bold));
    }

    #[test]
    fn a_file_with_no_known_blocks_replaces_the_page() {
        let mut page = page_of(&["old one", "old two"]);
        let base = page.version();
        let file = vec![Wanted {
            id: None,
            kind: BlockKind::Paragraph,
            indent: 0,
            spans: vec![Span::plain("all new")],
        }];
        merge(&mut page, &base, &file).unwrap();
        assert_eq!(texts(&page), ["all new"]);
    }

    /// Random edits in the app and in the file, made concurrently: every word either side
    /// typed is in the page afterwards.
    #[test]
    fn concurrent_edits_never_lose_typed_text() {
        for seed in 0..400 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let lines: Vec<String> = (0..rng.usize(1..6))
                .map(|i| format!("line{i} words here"))
                .collect();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let mut page = page_of(&refs);
            let base = page.version();
            let mut file = wanted(&page);

            // The app types tokens into random blocks.
            let mut typed = Vec::new();
            for n in 0..rng.usize(0..4) {
                let order = page.order();
                let block = order[rng.usize(..order.len())];
                let len = page.text_len(block).unwrap();
                let token = format!(" APP{seed}x{n} ");
                // At either end, so one token never splits another.
                let at = if rng.bool() { 0 } else { len };
                page.insert_text(block, at, &token).unwrap();
                typed.push(token.trim().to_owned());
            }
            // The file deletes and types, moves and drops blocks.
            for n in 0..rng.usize(0..5) {
                if file.is_empty() {
                    break;
                }
                let i = rng.usize(..file.len());
                match rng.u8(..4) {
                    0 => {
                        let mut text: Vec<char> =
                            file[i].spans.iter().flat_map(|s| s.text.chars()).collect();
                        if !text.is_empty() {
                            let at = rng.usize(..text.len());
                            let len = rng.usize(..=text.len() - at);
                            text.drain(at..at + len);
                        }
                        file[i].spans = vec![Span::plain(text.into_iter().collect::<String>())];
                    }
                    1 => {
                        let token = format!("FILE{seed}x{n}");
                        file[i].spans.push(Span::plain(format!(" {token}")));
                        typed.push(token);
                    }
                    2 => {
                        let b = file.remove(i);
                        let j = rng.usize(..=file.len());
                        file.insert(j, b);
                    }
                    _ => {
                        file.remove(i);
                    }
                }
            }
            // Only what the file still holds at the end counts as typed there.
            let kept: String = file
                .iter()
                .flat_map(|w| w.spans.iter().map(|s| s.text.as_str()))
                .collect();
            typed.retain(|t| !t.starts_with("FILE") || kept.contains(t.as_str()));
            merge(&mut page, &base, &file).unwrap();
            let all: String = texts(&page).join("\n");
            for t in &typed {
                assert!(
                    all.contains(t.as_str()),
                    "seed {seed}: {t} lost from {all:?}"
                );
            }
        }
    }

    #[test]
    fn longest_increasing_keeps_the_most() {
        assert_eq!(longest_increasing(&[3, 0, 1, 2]), [0, 1, 2]);
        assert_eq!(longest_increasing(&[]), Vec::<usize>::new());
        assert_eq!(longest_increasing(&[2, 1, 0]).len(), 1);
    }
}
