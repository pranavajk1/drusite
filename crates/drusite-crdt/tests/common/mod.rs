// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(dead_code, unreachable_pub)]

use drusite_crdt::Page;
use drusite_model::{BlockId, BlockKind, MAX_INDENT};

/// A second device: a copy of `page` loaded from its changes, as sync would make it.
pub fn fork(page: &Page) -> Page {
    Page::load(&page.export_snapshot().unwrap()).unwrap()
}

/// Sends `from`'s changes to `to`.
pub fn sync_one_way(from: &Page, to: &mut Page) {
    let bytes = from.export_updates(&to.version()).unwrap();
    to.import(&bytes).unwrap();
}

pub fn sync(a: &mut Page, b: &mut Page) {
    sync_one_way(a, b);
    sync_one_way(b, a);
}

/// The page as `p:text | h2:text | …`, for comparing outcomes.
pub fn shape(page: &Page) -> String {
    page.blocks()
        .unwrap()
        .iter()
        .map(|b| {
            let kind = match &b.kind {
                BlockKind::Heading { level: l } => format!("h{}", l.number()),
                k => k.name()[..1].to_string(),
            };
            let indent = if b.indent > 0 {
                format!("{}>", b.indent)
            } else {
                String::new()
            };
            format!("{indent}{kind}:{}", b.text())
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

pub fn texts(page: &Page) -> Vec<String> {
    page.blocks().unwrap().iter().map(|b| b.text()).collect()
}

/// A page with one paragraph holding `text`, and that paragraph's ID.
pub fn page_with(text: &str) -> (Page, BlockId) {
    let mut page = Page::create().unwrap();
    let first = page.order()[0];
    page.insert_text(first, 0, text).unwrap();
    (page, first)
}

/// A page with one paragraph per string.
pub fn page_with_blocks(texts: &[&str]) -> (Page, Vec<BlockId>) {
    let (mut page, first) = page_with(texts[0]);
    let mut ids = vec![first];
    for t in &texts[1..] {
        let (id, _) = page
            .insert_block_after(ids.last().copied(), BlockKind::Paragraph)
            .unwrap();
        page.insert_text(id, 0, t).unwrap();
        ids.push(id);
    }
    (page, ids)
}

/// Checks what must hold of any page: every block shows once, and indents never skip a level.
pub fn assert_well_formed(page: &Page) {
    let blocks = page.blocks().unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut prev: Option<u8> = None;
    for b in &blocks {
        assert!(seen.insert(b.id), "block {} shown twice", b.id);
        assert!(
            b.indent <= prev.map_or(0, |p| p + 1),
            "indent skips a level at {}",
            b.id
        );
        assert!(b.indent <= MAX_INDENT);
        prev = Some(b.indent);
    }
    assert!(!blocks.is_empty(), "a page always shows at least one block");
}

/// UTF-16 positions in `text` that aren't inside a character.
pub fn boundaries(text: &str) -> Vec<usize> {
    let mut out = vec![0];
    let mut at = 0;
    for ch in text.chars() {
        at += ch.len_utf16();
        out.push(at);
    }
    out
}
