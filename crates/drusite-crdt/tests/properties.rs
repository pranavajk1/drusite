// SPDX-License-Identifier: AGPL-3.0-or-later

//! Property tests: random concurrent edits on several devices always converge, and no text
//! anyone typed is ever lost to a merge.

// Test helpers panic on failure by design.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;

use common::*;
use drusite_crdt::{JoinOutcome, Page};
use drusite_model::{BlockKind, HeadingLevel, Mark};
use proptest::prelude::*;

const INSERTS: [&str; 8] = ["a", "bc", " ", "é", "日本", "🙂", "z\n", "x"];

/// An edit chosen by numbers, resolved against whatever the device's page looks like when it
/// runs, so every generated edit is a valid one and shrinking still works.
#[derive(Debug, Clone)]
struct Step {
    device: usize,
    op: u8,
    block: usize,
    pos: usize,
    other: usize,
    sync_to: Option<usize>,
}

fn steps(devices: usize, ops: u8) -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        (
            0..devices,
            0..ops,
            any::<usize>(),
            any::<usize>(),
            any::<usize>(),
            prop::option::weighted(0.2, 0..devices),
        )
            .prop_map(|(device, op, block, pos, other, sync_to)| Step {
                device,
                op,
                block,
                pos,
                other,
                sync_to,
            }),
        1..120,
    )
}

#[derive(Debug, Default)]
struct Tally {
    typed: HashMap<char, usize>,
    copied_joins: usize,
}

/// Runs one step. Ops 0–7 never delete text; 8 and 9 do.
fn apply(page: &mut Page, step: &Step, tally: &mut Tally) {
    let order = page.order();
    let id = order[step.block % order.len()];
    let cuts = boundaries(&page.block(id).unwrap().text());
    let pos = cuts[step.pos % cuts.len()];
    match step.op {
        0 | 1 => {
            let text = INSERTS[step.other % INSERTS.len()];
            page.insert_text(id, pos, text).unwrap();
            for c in text.chars() {
                *tally.typed.entry(c).or_default() += 1;
            }
        }
        2 => drop(page.split(id, pos, BlockKind::Paragraph).unwrap()),
        3 => {
            if page.join(id).unwrap().0 == JoinOutcome::Copied {
                tally.copied_joins += 1;
            }
        }
        4 => {
            let kinds = [
                BlockKind::Paragraph,
                BlockKind::Heading(HeadingLevel::H1),
                BlockKind::Quote,
                BlockKind::Toggle,
            ];
            page.set_kind(id, &kinds[step.other % kinds.len()]).unwrap();
        }
        5 => {
            let end = cuts[(step.pos % cuts.len() + 1 + step.other % 3).min(cuts.len() - 1)];
            page.format(id, pos, end, &Mark::Bold, step.other.is_multiple_of(2))
                .unwrap();
        }
        6 => {
            let after = (!step.other.is_multiple_of(4)).then(|| order[step.other % order.len()]);
            page.move_block(id, after).unwrap();
        }
        7 => drop(
            page.set_indent(id, u8::try_from(step.other % 4).unwrap())
                .unwrap(),
        ),
        8 => {
            let end = cuts[(step.pos % cuts.len() + 1 + step.other % 3).min(cuts.len() - 1)];
            page.delete_text(id, pos, end - pos).unwrap();
        }
        _ => page.delete_block(id).map(drop).unwrap(),
    }
}

fn run(steps: &[Step], devices: usize) -> (Vec<Page>, Tally, String) {
    let (genesis, _) = page_with("seed");
    let initial = texts(&genesis).concat();
    let mut pages: Vec<Page> = (0..devices).map(|_| fork(&genesis)).collect();
    let mut tally = Tally::default();
    for step in steps {
        apply(&mut pages[step.device], step, &mut tally);
        if let Some(to) = step.sync_to.filter(|&t| t != step.device) {
            let bytes = pages[step.device]
                .export_updates(&pages[to].version())
                .unwrap();
            pages[to].import(&bytes).unwrap();
        }
    }
    for _ in 0..2 {
        for i in 0..devices {
            for j in 0..devices {
                if i != j {
                    let bytes = pages[i].export_updates(&pages[j].version()).unwrap();
                    pages[j].import(&bytes).unwrap();
                }
            }
        }
    }
    (pages, tally, initial)
}

fn char_counts(text: &str) -> HashMap<char, usize> {
    let mut counts = HashMap::new();
    for c in text.chars() {
        *counts.entry(c).or_default() += 1;
    }
    counts
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every kind of edit, deletes included: all devices end up with the same page, the same
    /// one a fresh load of the merged document shows.
    #[test]
    fn concurrent_edits_converge(steps in steps(3, 10)) {
        let (pages, _, _) = run(&steps, 3);
        let expected = fork(&pages[0]).blocks().unwrap();
        for page in &pages {
            prop_assert_eq!(&page.blocks().unwrap(), &expected);
            assert_well_formed(page);
        }
    }

    /// No deletes: every character anyone typed is in the merged page, exactly once (or more
    /// than once only if a join had to copy text between moved blocks).
    #[test]
    fn concurrent_edits_lose_no_text(steps in steps(3, 8)) {
        let (pages, tally, initial) = run(&steps, 3);
        let have = char_counts(&texts(&pages[0]).concat());
        let mut want = char_counts(&initial);
        for (c, n) in &tally.typed {
            *want.entry(*c).or_default() += n;
        }
        for (c, n) in &want {
            let got = have.get(c).copied().unwrap_or(0);
            prop_assert!(got >= *n, "lost {:?}: typed {} times, {} in the page", c, n, got);
            if tally.copied_joins == 0 {
                prop_assert_eq!(got, *n, "{:?} duplicated", c);
            }
        }
        if tally.copied_joins == 0 {
            prop_assert_eq!(have.len(), want.len(), "characters nobody typed");
        }
    }

    /// Two devices only, deletes included, every edit synced straight away half the time: the
    /// shape most real use takes.
    #[test]
    fn two_devices_converge(steps in steps(2, 10)) {
        let (pages, _, _) = run(&steps, 2);
        prop_assert_eq!(pages[0].blocks().unwrap(), pages[1].blocks().unwrap());
    }
}
