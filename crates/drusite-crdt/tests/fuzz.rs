// SPDX-License-Identifier: AGPL-3.0-or-later

//! Spike 2's merge fuzz, ported: three devices make random edits of every kind, sync with each
//! other at random (one way, partially), then sync fully. They must all end up showing the same
//! page, the same one a fresh load of the merged document shows, and a well-formed one.
//!
//! `FUZZ_SEEDS` and `FUZZ_STEPS` raise the effort (defaults 30 seeds × 200 steps, as in spike 2).

// Test helpers panic on failure by design.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use drusite_crdt::{Error, Page};
use drusite_model::{BlockKind, HeadingLevel, Mark};

const INSERTS: [&str; 6] = ["a", "bb", " ", "é", "日本", "🙂"];

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One random edit on `page`. Refusals the API is allowed to make are fine; anything else fails.
fn random_edit(page: &mut Page, rng: &mut fastrand::Rng) {
    let order = page.order();
    let id = order[rng.usize(..order.len())];
    let text = page.block(id).unwrap().text();
    let cuts = boundaries(&text);
    let pos = cuts[rng.usize(..cuts.len())];
    let op = rng.u8(..10);
    let sub = rng.u8(..3);
    let result = match op {
        0..=2 => page
            .insert_text(id, pos, INSERTS[rng.usize(..INSERTS.len())])
            .map(drop),
        3 => {
            let end = cuts
                .iter()
                .copied()
                .filter(|&c| c > pos)
                .take(4)
                .nth(rng.usize(..4))
                .unwrap_or(pos);
            page.delete_text(id, pos, end - pos).map(drop)
        }
        4 => page.split(id, pos, BlockKind::Paragraph).map(drop),
        5 => page.join(id).map(drop),
        6 => {
            let kind = match rng.u8(..4) {
                0 => BlockKind::Paragraph,
                1 => BlockKind::Heading {
                    level: HeadingLevel::H2,
                },
                2 => BlockKind::Todo {
                    checked: rng.bool(),
                },
                _ => BlockKind::Bulleted,
            };
            page.set_kind(id, &kind).map(drop)
        }
        7 => {
            let end = cuts
                .iter()
                .copied()
                .filter(|&c| c > pos)
                .take(3)
                .last()
                .unwrap_or(pos);
            let mark = if rng.bool() { Mark::Bold } else { Mark::Italic };
            page.format(id, pos, end, &mark, rng.u8(..4) > 0).map(drop)
        }
        8 => {
            let after = if rng.u8(..5) == 0 {
                None
            } else {
                Some(order[rng.usize(..order.len())])
            };
            page.move_block(id, after).map(drop)
        }
        _ => match sub {
            0 => page.set_indent(id, rng.u8(..4)).map(drop),
            1 => page.delete_block(id).map(drop),
            _ => {
                // Now and then at the top of the page, as a paste over the first block does.
                let after = if rng.u8(..4) == 0 { None } else { Some(id) };
                page.insert_block_after(after, BlockKind::Paragraph)
                    .map(drop)
            }
        },
    };
    match &result {
        Ok(()) | Err(Error::OutOfRange { .. } | Error::InsideCharacter(_)) => {}
        Err(err) => panic!("edit failed: {err}"),
    }
    assert!(
        page.layout_is_consistent(),
        "incremental layout drifted from a full recompute after op {op}/{sub}: {result:?}"
    );
}

#[test]
fn three_devices_random_edits_converge() {
    let seeds = env("FUZZ_SEEDS", 30);
    let steps = env("FUZZ_STEPS", 200);
    for seed in 1..=seeds {
        let mut rng = fastrand::Rng::with_seed(seed);
        let (d0, _) = page_with("seed text here");
        let mut devices = [fork(&d0), fork(&d0), d0];
        for _ in 0..steps {
            let d = rng.usize(..3);
            random_edit(&mut devices[d], &mut rng);
            if rng.u8(..8) == 0 {
                let from = rng.usize(..3);
                let to = (from + 1 + rng.usize(..2)) % 3;
                let bytes = devices[from]
                    .export_updates(&devices[to].version())
                    .unwrap();
                devices[to].import(&bytes).unwrap();
                assert!(devices[to].layout_is_consistent());
            }
        }
        for _ in 0..2 {
            for (i, j) in [(0, 1), (1, 2), (0, 2)] {
                let (left, right) = devices.split_at_mut(j);
                sync(&mut left[i], &mut right[0]);
            }
        }
        let fresh = fork(&devices[0]);
        let expected = fresh.blocks().unwrap();
        for (i, d) in devices.iter().enumerate() {
            assert_eq!(
                d.blocks().unwrap(),
                expected,
                "seed {seed}: device {i} differs from the merged document"
            );
            assert_well_formed(d);
        }
    }
}

/// The same edits on long pages, two devices apart. The layout keeps its block lists in chunks
/// of about √n blocks; a page this long has many of them, so edits split, merge and re-cut
/// chunks, and every one must still leave the layout exactly as a full recompute would.
#[test]
fn long_page_random_edits_keep_the_layout() {
    let seeds = env("FUZZ_SEEDS", 30) / 10;
    let steps = env("FUZZ_STEPS", 200) * 3;
    for seed in 1..=seeds {
        let mut rng = fastrand::Rng::with_seed(seed);
        let texts: Vec<String> = (0..300 + rng.usize(..300))
            .map(|i| format!("block {i}"))
            .collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let (d0, _) = page_with_blocks(&texts);
        let mut devices = [fork(&d0), d0];
        for _ in 0..steps {
            let d = rng.usize(..2);
            random_edit(&mut devices[d], &mut rng);
            if rng.u8(..16) == 0 {
                let (a, b) = devices.split_at_mut(1);
                sync(&mut a[0], &mut b[0]);
                assert!(a[0].layout_is_consistent() && b[0].layout_is_consistent());
            }
        }
        let (a, b) = devices.split_at_mut(1);
        sync(&mut a[0], &mut b[0]);
        assert_eq!(
            a[0].blocks().unwrap(),
            b[0].blocks().unwrap(),
            "seed {seed}"
        );
        assert_eq!(
            a[0].blocks().unwrap(),
            fork(&a[0]).blocks().unwrap(),
            "seed {seed}"
        );
        assert_well_formed(&a[0]);
    }
}
