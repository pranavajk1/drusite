// SPDX-License-Identifier: AGPL-3.0-or-later

//! Taking one change back (an agent's edit) while keeping everything around it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use drusite_crdt::{Page, Workspace};
use drusite_model::BlockKind;

fn page_with(texts: &[&str]) -> Page {
    let mut page = Page::create().unwrap();
    let first = page.order()[0];
    page.insert_text(first, 0, texts[0]).unwrap();
    let mut last = first;
    for t in &texts[1..] {
        let (id, _) = page
            .insert_block_after(Some(last), BlockKind::Paragraph)
            .unwrap();
        page.insert_text(id, 0, t).unwrap();
        last = id;
    }
    page
}

#[test]
fn an_edit_is_taken_back_and_later_typing_stays() {
    let mut page = page_with(&["one", "two"]);
    let [a, b] = page.order()[..] else { panic!() };
    let before = page.version();
    // The agent adds a block and changes a word.
    let (added, _) = page
        .insert_block_after(Some(b), BlockKind::Bulleted)
        .unwrap();
    page.insert_text(added, 0, "from the agent").unwrap();
    page.insert_text(a, 3, " (agent)").unwrap();
    let after = page.version();
    // Then the person types, in a block the agent touched and one it didn't.
    page.insert_text(a, 0, ">> ").unwrap();
    page.insert_text(b, 3, "!").unwrap();

    page.revert_change(&before, &after).unwrap();
    assert_eq!(shape(&page), "p:>> one | p:two!");
    assert!(page.layout_is_consistent());
}

#[test]
fn a_deleted_block_comes_back_as_it_was() {
    let mut page = page_with(&["keep", "gone", "also"]);
    let gone = page.order()[1];
    let before = page.version();
    page.set_kind(gone, &BlockKind::Todo { checked: true })
        .unwrap();
    page.delete_block(gone).unwrap();
    let after = page.version();
    page.revert_change(&before, &after).unwrap();
    assert_eq!(shape(&page), "p:keep | p:gone | p:also");
    assert!(page.order().contains(&gone), "the block keeps its ID");
}

#[test]
fn an_undo_reaches_other_devices_and_they_converge() {
    let mut page = page_with(&["shared"]);
    let mut other = fork(&page);
    let id = page.order()[0];
    let before = page.version();
    page.insert_text(id, 6, " text").unwrap();
    let after = page.version();
    sync(&mut page, &mut other);
    // The other device types while the undo happens here.
    other.insert_text(id, 0, "my ").unwrap();
    page.revert_change(&before, &after).unwrap();
    sync(&mut page, &mut other);
    assert_eq!(shape(&page), "p:my shared");
    assert_eq!(shape(&page), shape(&other));
}

#[test]
fn undo_of_random_edits_restores_the_page_when_nothing_came_after() {
    for seed in 0..40 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let mut page = page_with(&["alpha beta", "gamma", "delta epsilon"]);
        for _ in 0..rng.usize(..10) {
            random_text_edit(&mut page, &mut rng);
        }
        let before = page.version();
        let expected = shape(&page);
        for _ in 0..rng.usize(1..12) {
            random_text_edit(&mut page, &mut rng);
        }
        let after = page.version();
        page.revert_change(&before, &after).unwrap();
        assert_eq!(shape(&page), expected, "seed {seed}");
        assert!(page.layout_is_consistent(), "seed {seed}");
        // And a fresh load agrees.
        assert_eq!(shape(&fork(&page)), expected, "seed {seed}");
    }
}

fn random_text_edit(page: &mut Page, rng: &mut fastrand::Rng) {
    let order = page.order();
    let id = order[rng.usize(..order.len())];
    let len = page.block(id).unwrap().text().chars().count();
    let pos = rng.usize(..=len);
    let _ = match rng.u8(..6) {
        0 | 1 => page.insert_text(id, pos, "xy").map(drop),
        2 if len > pos => page.delete_text(id, pos, 1).map(drop),
        3 => page.split(id, pos, BlockKind::Paragraph).map(drop),
        4 if order.len() > 1 => page.delete_block(id).map(drop),
        _ => page
            .move_block(id, Some(order[rng.usize(..order.len())]))
            .map(drop),
    };
}

#[test]
fn a_title_change_is_taken_back_and_a_later_one_stays() {
    let mut ws = Workspace::create().unwrap();
    let a = ws.create_page(None, "Plans").unwrap().id();
    let b = ws.create_page(None, "Ideas").unwrap().id();
    let before = ws.version();
    ws.set_title(a, "Plans (edited by agent)").unwrap();
    let after = ws.version();
    ws.set_title(b, "Ideas, renamed by me").unwrap();
    ws.revert_change(&before, &after).unwrap();
    assert_eq!(ws.title(a).unwrap(), "Plans");
    assert_eq!(ws.title(b).unwrap(), "Ideas, renamed by me");
}
