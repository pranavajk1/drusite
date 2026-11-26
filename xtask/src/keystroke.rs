// SPDX-License-Identifier: AGPL-3.0-or-later

//! The keystroke budget: what one key costs the core in a long page, through `drusite-crdt`'s
//! own edit API: typing a character, Enter (a split) and Backspace at the start of a block (a
//! join). Each one includes exporting the update the store appends.

use std::hint::black_box;
use std::time::Instant;

use anyhow::Result;
use drusite_crdt::Page;
use drusite_model::{BlockId, BlockKind};

use crate::budgets::{
    KEYSTROKE_BLOCKS, KEYSTROKE_P95_NANOS, KEYSTROKE_SCALING_MAX, Report, fmt_nanos,
};

const SMALL_PAGE_BLOCKS: usize = 100;
const WARMUP: usize = 100;
const SAMPLES: usize = 1_000;

#[derive(Debug, Clone, Copy)]
enum Key {
    Type,
    Enter,
    Backspace,
}

impl Key {
    const ALL: [Key; 3] = [Key::Type, Key::Enter, Key::Backspace];

    fn name(self) -> &'static str {
        match self {
            Key::Type => "typing",
            Key::Enter => "Enter",
            Key::Backspace => "Backspace",
        }
    }
}

pub(crate) fn check(report: &mut Report) -> Result<()> {
    for key in Key::ALL {
        let small = measure(SMALL_PAGE_BLOCKS, key)?;
        let large = measure(KEYSTROKE_BLOCKS, key)?;
        report.record(
            format!("{} p95, {KEYSTROKE_BLOCKS} blocks", key.name()),
            fmt_nanos(large.p95),
            format!("≤ {}", fmt_nanos(KEYSTROKE_P95_NANOS)),
            large.p95 <= KEYSTROKE_P95_NANOS,
        );
        let ratio = large.median as f64 / small.median.max(1) as f64;
        report.record(
            format!(
                "{} median, {KEYSTROKE_BLOCKS} vs {SMALL_PAGE_BLOCKS} blocks",
                key.name()
            ),
            format!(
                "{ratio:.2}× ({} vs {})",
                fmt_nanos(large.median),
                fmt_nanos(small.median)
            ),
            format!("≤ {KEYSTROKE_SCALING_MAX:.1}×"),
            ratio <= KEYSTROKE_SCALING_MAX,
        );
    }
    report.note(format!(
        "keystrokes: {SAMPLES} samples per key and page size, through drusite-crdt's edit API, \
         update export included. Measures the core only; IPC and the editor join in Phase 1 step 5."
    ));
    Ok(())
}

#[derive(Debug)]
struct Stats {
    median: u64,
    p95: u64,
}

fn page(blocks: usize) -> Result<(Page, Vec<BlockId>)> {
    let mut page = Page::create()?;
    let mut ids = vec![page.order()[0]];
    page.insert_text(ids[0], 0, &sample_text(0))?;
    for i in 1..blocks {
        let (id, _) = page.insert_block_after(ids.last().copied(), BlockKind::Paragraph)?;
        page.insert_text(id, 0, &sample_text(i))?;
        ids.push(id);
    }
    Ok((page, ids))
}

fn measure(blocks: usize, key: Key) -> Result<Stats> {
    let (mut page, ids) = page(blocks)?;
    let mut rng = fastrand::Rng::with_seed(0xD5_5173);
    let mut samples = Vec::with_capacity(SAMPLES);
    for round in 0..WARMUP + SAMPLES {
        // Anywhere in the page, but not the first block (Backspace there does nothing).
        let id = ids[rng.usize(1..ids.len())];
        let since = page.version();
        let start = Instant::now();
        match key {
            Key::Type => {
                let len = page.text_len(id)?;
                page.insert_text(id, rng.usize(..=len), "x")?;
            }
            Key::Enter => {
                let len = page.text_len(id)?;
                let (new, _) = page.split(id, rng.usize(..=len), BlockKind::Paragraph)?;
                black_box(new);
            }
            Key::Backspace => {
                // Split first (untimed below), so each sample joins a fresh pair.
            }
        }
        let update = page.export_updates(&since)?;
        let mut elapsed = start.elapsed();
        if let Key::Backspace = key {
            let len = page.text_len(id)?;
            let (new, _) = page.split(id, len / 2, BlockKind::Paragraph)?;
            let since = page.version();
            let start = Instant::now();
            page.join(new)?;
            black_box(page.export_updates(&since)?);
            elapsed = start.elapsed();
        }
        black_box(update);
        if round >= WARMUP {
            samples.push(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
        }
    }
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    Ok(Stats {
        median: at(0.5),
        p95: at(0.95),
    })
}

fn sample_text(i: usize) -> String {
    const WORDS: [&str; 12] = [
        "drusite", "notes", "crystal", "block", "page", "sync", "loro", "merge", "quartz", "layer",
        "hollow", "rock",
    ];
    (0..8)
        .map(|k| WORDS[(i * 7 + k * 5) % WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ")
}
