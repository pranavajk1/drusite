// SPDX-License-Identifier: AGPL-3.0-or-later

//! Two devices editing the same page, merged. Spike 2's merge tests, its known-loss cases (which
//! must now come out the way the person meant), and the cases this block model adds: moves.

// Test helpers panic on failure by design.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use drusite_crdt::{Error, JoinOutcome, Page};
use drusite_model::{BlockKind, HeadingLevel, Mark};

#[test]
fn concurrent_typing_in_one_paragraph_keeps_both_edits() {
    let (mut a, first) = page_with("hello world");
    let mut b = fork(&a);
    a.insert_text(first, 5, " brave").unwrap();
    b.insert_text(first, 11, "!").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(texts(&a), ["hello brave world!"]);
    assert_eq!(shape(&a), shape(&b));
}

#[test]
fn offline_structural_edits_on_both_sides_merge() {
    let (mut a, first) = page_with("Shared intro");
    let mut b = fork(&a);
    // a, offline: three paragraphs at the end and a title at the top.
    let mut last = first;
    for i in 0..3 {
        let (id, _) = a
            .insert_block_after(Some(last), BlockKind::Paragraph)
            .unwrap();
        a.insert_text(id, 0, &format!("a offline {i}")).unwrap();
        last = id;
    }
    let (title, _) = a
        .insert_block_after(None, BlockKind::Heading(HeadingLevel::H1))
        .unwrap();
    a.insert_text(title, 0, "A's title").unwrap();
    // b, offline: edits the shared paragraph and adds a bullet.
    b.insert_text(first, 6, " (edited by b)").unwrap();
    let (bullet, _) = b
        .insert_block_after(Some(first), BlockKind::Bulleted)
        .unwrap();
    b.insert_text(bullet, 0, "b bullet").unwrap();

    sync(&mut a, &mut b);
    assert_eq!(shape(&a), shape(&b));
    let all = texts(&a);
    for want in [
        "A's title",
        "Shared (edited by b) intro",
        "a offline 0",
        "a offline 2",
        "b bullet",
    ] {
        assert!(all.iter().any(|t| t == want), "missing {want:?} in {all:?}");
    }
    assert_eq!(all[0], "A's title");
    assert_well_formed(&a);
}

#[test]
fn text_typed_into_a_block_deleted_elsewhere_is_kept() {
    let (mut a, ids) = page_with_blocks(&["first", "second para"]);
    let mut b = fork(&a);
    a.delete_block(ids[1]).unwrap();
    b.insert_text(ids[1], 6, " typed-into").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(shape(&a), shape(&b));
    assert!(
        texts(&a).concat().contains(" typed-into"),
        "lost: {}",
        shape(&a)
    );
}

#[test]
fn concurrent_bold_and_insert_inside_the_bold_range() {
    let (mut a, first) = page_with("make this bold");
    let mut b = fork(&a);
    a.format(first, 5, 9, &Mark::Bold, true).unwrap();
    b.insert_text(first, 7, "XX").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(a.blocks().unwrap(), b.blocks().unwrap());
    let block = a.block(first).unwrap();
    assert_eq!(block.text(), "make thXXis bold");
    let bold: String = block
        .spans
        .iter()
        .filter(|s| s.marks.bold)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(bold, "thXXis");
}

/// Spike 2's hazard: two devices that each set up the same empty page lost one side. Here a
/// page can only be created once, so two creations are two pages, and mixing them is refused.
#[test]
fn a_page_is_created_once_and_another_pages_changes_are_refused() {
    let (mut a, _) = page_with("from a");
    let (b, _) = page_with("from b");
    assert_ne!(a.id(), b.id());
    let err = a.import(&b.export_snapshot().unwrap()).unwrap_err();
    assert!(matches!(err, Error::WrongDocument), "{err}");
    assert!(a.is_poisoned());
}

#[test]
fn known_loss_concurrent_splits_of_one_paragraph() {
    let (mut a, first) = page_with("abcdef");
    let mut b = fork(&a);
    a.split(first, 2, BlockKind::Paragraph).unwrap(); // ab | cdef
    b.split(first, 4, BlockKind::Paragraph).unwrap(); // abcd | ef
    sync(&mut a, &mut b);
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(texts(&a), ["ab", "cd", "ef"]);
}

#[test]
fn known_loss_typing_into_a_block_while_it_becomes_a_heading() {
    let (mut a, first) = page_with("abcdef");
    let mut b = fork(&a);
    a.set_kind(first, &BlockKind::Heading(HeadingLevel::H2))
        .unwrap();
    b.insert_text(first, 2, "XX").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(shape(&a), "h2:abXXcdef");
}

#[test]
fn known_loss_typing_after_the_split_point_while_the_paragraph_is_split() {
    let (mut a, first) = page_with("abcdef");
    let mut b = fork(&a);
    a.split(first, 3, BlockKind::Paragraph).unwrap(); // abc | def
    b.insert_text(first, 5, "X").unwrap(); // abcdeXf
    sync(&mut a, &mut b);
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(texts(&a), ["abc", "deXf"]);
}

#[test]
fn join_while_the_joined_block_is_typed_into() {
    let (mut a, ids) = page_with_blocks(&["abc", "def"]);
    let mut b = fork(&a);
    let (outcome, _) = a.join(ids[1]).unwrap();
    assert_eq!(outcome, JoinOutcome::Merged);
    b.insert_text(ids[1], 1, "Y").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(texts(&a), ["abcdYef"]);
    assert_eq!(shape(&a), shape(&b));
}

#[test]
fn a_third_device_loads_from_a_snapshot_and_matches() {
    let (mut a, first) = page_with("abc");
    let mut b = fork(&a);
    a.insert_text(first, 3, "xyz").unwrap();
    b.split(first, 1, BlockKind::Quote).unwrap();
    sync(&mut a, &mut b);
    let c = fork(&a);
    assert_eq!(a.blocks().unwrap(), c.blocks().unwrap());
}

#[test]
fn text_typed_into_a_block_follows_it_when_it_moves_elsewhere() {
    let (mut a, ids) = page_with_blocks(&["one", "two", "three"]);
    let mut b = fork(&a);
    a.move_block(ids[0], Some(ids[2])).unwrap();
    assert_eq!(texts(&a), ["two", "three", "one"]);
    b.insert_text(ids[0], 3, "!").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(texts(&a), ["two", "three", "one!"]);
    assert_eq!(shape(&a), shape(&b));
}

#[test]
fn concurrent_moves_of_one_block_leave_it_in_one_place() {
    let (mut a, ids) = page_with_blocks(&["one", "two", "three", "four"]);
    let mut b = fork(&a);
    a.move_block(ids[1], Some(ids[3])).unwrap();
    b.move_block(ids[1], None).unwrap();
    sync(&mut a, &mut b);
    assert_eq!(shape(&a), shape(&b));
    let all = texts(&a);
    assert_eq!(all.iter().filter(|t| *t == "two").count(), 1, "{all:?}");
    assert_eq!(all.len(), 4);
    assert_well_formed(&a);
}

#[test]
fn moving_a_block_that_is_being_split_elsewhere_keeps_all_its_text() {
    let (mut a, ids) = page_with_blocks(&["one", "abcdef", "three"]);
    let mut b = fork(&a);
    a.move_block(ids[1], None).unwrap();
    b.split(ids[1], 3, BlockKind::Paragraph).unwrap();
    sync(&mut a, &mut b);
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(texts(&a), ["abc", "def", "one", "three"]);
}

#[test]
fn moves_land_exactly_where_asked() {
    let (mut page, ids) = page_with_blocks(&["0", "1", "2", "3", "4"]);
    page.move_block(ids[3], Some(ids[0])).unwrap();
    assert_eq!(texts(&page), ["0", "3", "1", "2", "4"]);
    page.move_block(ids[0], Some(ids[4])).unwrap();
    assert_eq!(texts(&page), ["3", "1", "2", "4", "0"]);
    page.move_block(ids[2], None).unwrap();
    assert_eq!(texts(&page), ["2", "3", "1", "4", "0"]);
    // Splitting a moved block puts the new block right after it.
    page.split(ids[2], 1, BlockKind::Paragraph).unwrap();
    assert_eq!(texts(&page), ["2", "", "3", "1", "4", "0"]);
    // Deleting a moved block leaves the others where they are.
    page.delete_block(ids[3]).unwrap();
    assert_eq!(texts(&page), ["2", "", "1", "4", "0"]);
    // Joining a block onto one that was moved next to it copies its text across.
    let (outcome, _) = page.join(ids[1]).unwrap();
    assert_eq!(outcome, JoinOutcome::Copied);
    assert_eq!(texts(&page), ["2", "1", "4", "0"]);
    assert_well_formed(&page);
    assert_eq!(fork(&page).blocks().unwrap(), page.blocks().unwrap());
}

#[test]
fn the_first_block_can_be_deleted_and_still_takes_text_typed_elsewhere() {
    let (mut a, ids) = page_with_blocks(&["first", "second"]);
    let mut b = fork(&a);
    a.delete_block(ids[0]).unwrap();
    assert_eq!(texts(&a), ["second"]);
    b.insert_text(ids[0], 5, " kept").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(texts(&a), [" kept", "second"]);
}

#[test]
fn positions_are_checked_before_anything_is_written() {
    let (mut page, first) = page_with("x🙂y");
    let before = page.export_snapshot().unwrap();
    assert!(matches!(
        page.insert_text(first, 9, "z"),
        Err(Error::OutOfRange { pos: 9, len: 4 })
    ));
    // Between the two halves of the emoji.
    assert!(matches!(
        page.insert_text(first, 2, "z"),
        Err(Error::InsideCharacter(2))
    ));
    assert!(matches!(
        page.delete_text(first, 1, 1),
        Err(Error::InsideCharacter(2))
    ));
    assert!(matches!(
        page.split(first, 2, BlockKind::Paragraph),
        Err(Error::InsideCharacter(2))
    ));
    let stranger = drusite_model::BlockId::new();
    assert!(matches!(
        page.insert_text(stranger, 0, "z"),
        Err(Error::UnknownBlock(_))
    ));
    assert!(!page.is_poisoned());
    assert_eq!(
        page.export_snapshot().unwrap(),
        before,
        "a refused edit wrote something"
    );
    // Whole characters are fine.
    page.delete_text(first, 1, 2).unwrap();
    assert_eq!(texts(&page), ["xy"]);
}

#[test]
fn the_block_marker_cannot_be_typed() {
    let (mut page, first) = page_with("a");
    page.insert_text(first, 1, &format!("b{}c", drusite_crdt::MARKER))
        .unwrap();
    assert_eq!(texts(&page), ["ab\u{FFFD}c"]);
    assert_eq!(page.order().len(), 1);
}

#[test]
fn indents_never_skip_a_level() {
    let (mut a, ids) = page_with_blocks(&["0", "1", "2"]);
    a.set_kind(ids[1], &BlockKind::Bulleted).unwrap();
    a.set_indent(ids[1], 4).unwrap();
    assert_eq!(
        a.block(ids[1]).unwrap().indent,
        1,
        "clamped to one deeper than the block before"
    );
    a.set_indent(ids[2], 2).unwrap();
    assert_eq!(a.block(ids[2]).unwrap().indent, 2);
    // Concurrently, the parent is outdented: the child is shown one level below it.
    let mut b = fork(&a);
    b.set_indent(ids[1], 0).unwrap();
    a.insert_text(ids[2], 1, "!").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(a.block(ids[2]).unwrap().indent, 1);
    assert_well_formed(&a);
}

#[test]
fn every_block_kind_survives_a_reload() {
    let kinds = [
        BlockKind::Paragraph,
        BlockKind::Heading(HeadingLevel::H1),
        BlockKind::Heading(HeadingLevel::H2),
        BlockKind::Heading(HeadingLevel::H3),
        BlockKind::Bulleted,
        BlockKind::Numbered,
        BlockKind::Todo { checked: true },
        BlockKind::Todo { checked: false },
        BlockKind::Quote,
        BlockKind::Code {
            language: "rust".into(),
        },
        BlockKind::Divider,
        BlockKind::Callout {
            icon: "💡".into()
        },
        BlockKind::Toggle,
    ];
    let mut page = Page::create().unwrap();
    let mut last = page.order()[0];
    for kind in &kinds {
        let (id, _) = page.insert_block_after(Some(last), kind.clone()).unwrap();
        last = id;
    }
    let loaded = fork(&page);
    let got: Vec<BlockKind> = loaded
        .blocks()
        .unwrap()
        .into_iter()
        .skip(1)
        .map(|b| b.kind)
        .collect();
    assert_eq!(got, kinds);
}

#[test]
fn a_keystroke_patch_names_only_the_block_typed_into() {
    let (mut page, ids) = page_with_blocks(&["one", "two", "three"]);
    let patch = page.insert_text(ids[1], 3, "!").unwrap();
    assert_eq!(patch.updated, [ids[1]]);
    assert!(patch.removed.is_empty() && !patch.order_changed);
    // The update a keystroke produces is small whatever the page's size.
    let v = page.version();
    page.insert_text(ids[2], 0, "x").unwrap();
    assert!(page.export_updates(&v).unwrap().len() < 200);
}

#[test]
fn long_page_5000_blocks() {
    let mut page = Page::create().unwrap();
    let mut last = page.order()[0];
    let start = std::time::Instant::now();
    for i in 0..5000 {
        let (id, _) = page
            .insert_block_after(Some(last), BlockKind::Paragraph)
            .unwrap();
        page.insert_text(
            id,
            0,
            &format!("Block {i}: the quick brown fox jumps over the lazy dog."),
        )
        .unwrap();
        last = id;
    }
    let build = start.elapsed();
    let snapshot = page.export_snapshot().unwrap();
    let start = std::time::Instant::now();
    let mut other = Page::load(&snapshot).unwrap();
    let load = start.elapsed();
    let start = std::time::Instant::now();
    page.insert_text(last, 3, "x").unwrap();
    sync_one_way(&page, &mut other);
    let edit = start.elapsed();
    assert_eq!(other.block(last).unwrap(), page.block(last).unwrap());
    println!(
        "5000 blocks: build {build:?}, snapshot {} KiB, load {load:?}, one edit + sync {edit:?}",
        snapshot.len() / 1024
    );
}
