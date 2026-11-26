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

/// Shared CI runners are noisy, so each key is measured up to this many times and the best
/// attempt counts. A real slowdown shows up in every attempt; a noisy neighbour doesn't.
const ATTEMPTS: usize = 3;

pub(crate) fn check(report: &mut Report) -> Result<()> {
    for key in Key::ALL {
        let mut best: Option<(Stats, Stats)> = None;
        for _ in 0..ATTEMPTS {
            let (small, large) = measure_pair(key)?;
            let better = best
                .as_ref()
                .is_none_or(|(s, l)| ratio(&small, &large) < ratio(s, l));
            if better {
                best = Some((small, large));
            }
            let (s, l) = best.as_ref().expect("set above");
            if ratio(s, l) <= KEYSTROKE_SCALING_MAX && l.p95 <= KEYSTROKE_P95_NANOS {
                break;
            }
        }
        let (small, large) = best.expect("at least one attempt");
        report.record(
            format!("{} p95, {KEYSTROKE_BLOCKS} blocks", key.name()),
            fmt_nanos(large.p95),
            format!("≤ {}", fmt_nanos(KEYSTROKE_P95_NANOS)),
            large.p95 <= KEYSTROKE_P95_NANOS,
        );
        let r = ratio(&small, &large);
        report.record(
            format!(
                "{} median, {KEYSTROKE_BLOCKS} vs {SMALL_PAGE_BLOCKS} blocks",
                key.name()
            ),
            format!(
                "{r:.2}× ({} vs {})",
                fmt_nanos(large.median),
                fmt_nanos(small.median)
            ),
            format!("≤ {KEYSTROKE_SCALING_MAX:.1}×"),
            r <= KEYSTROKE_SCALING_MAX,
        );
    }
    report.note(format!(
        "keystrokes: {SAMPLES} samples per key and page size, the two page sizes interleaved, best \
         of up to {ATTEMPTS} attempts; through drusite-crdt's edit API, update export included. \
         The core only: IPC and the editor aren't included."
    ));
    Ok(())
}

fn ratio(small: &Stats, large: &Stats) -> f64 {
    large.median as f64 / small.median.max(1) as f64
}

#[derive(Debug)]
struct Stats {
    median: u64,
    p95: u64,
}

/// One page being typed into, sample by sample.
struct Bench {
    page: Page,
    ids: Vec<BlockId>,
    rng: fastrand::Rng,
    samples: Vec<u64>,
}

impl Bench {
    fn new(blocks: usize) -> Result<Self> {
        let mut page = Page::create()?;
        let mut ids = vec![page.order()[0]];
        page.insert_text(ids[0], 0, &sample_text(0))?;
        for i in 1..blocks {
            let (id, _) = page.insert_block_after(ids.last().copied(), BlockKind::Paragraph)?;
            page.insert_text(id, 0, &sample_text(i))?;
            ids.push(id);
        }
        Ok(Self {
            page,
            ids,
            rng: fastrand::Rng::with_seed(0xD5_5173),
            samples: Vec::new(),
        })
    }

    /// One key, timed. Kept only after the warm-up.
    fn step(&mut self, key: Key, keep: bool) -> Result<()> {
        let page = &mut self.page;
        // Anywhere in the page, but not the first block (Backspace there does nothing).
        let id = self.ids[self.rng.usize(1..self.ids.len())];
        let elapsed = match key {
            Key::Type => {
                let len = page.text_len(id)?;
                let pos = self.rng.usize(..=len);
                let since = page.version();
                let start = Instant::now();
                page.insert_text(id, pos, "x")?;
                black_box(page.export_updates(&since)?);
                start.elapsed()
            }
            Key::Enter => {
                let len = page.text_len(id)?;
                let pos = self.rng.usize(..=len);
                let since = page.version();
                let start = Instant::now();
                let (new, _) = page.split(id, pos, BlockKind::Paragraph)?;
                black_box(new);
                black_box(page.export_updates(&since)?);
                start.elapsed()
            }
            Key::Backspace => {
                // Split first (untimed), so each sample joins a fresh pair.
                let len = page.text_len(id)?;
                let (new, _) = page.split(id, len / 2, BlockKind::Paragraph)?;
                let since = page.version();
                let start = Instant::now();
                page.join(new)?;
                black_box(page.export_updates(&since)?);
                start.elapsed()
            }
        };
        if keep {
            self.samples
                .push(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
        }
        Ok(())
    }

    fn stats(mut self) -> Stats {
        self.samples.sort_unstable();
        let at = |q: f64| self.samples[((self.samples.len() - 1) as f64 * q).round() as usize];
        Stats {
            median: at(0.5),
            p95: at(0.95),
        }
    }
}

/// The small and the large page, measured in alternation so noise lands on both alike.
fn measure_pair(key: Key) -> Result<(Stats, Stats)> {
    let mut small = Bench::new(SMALL_PAGE_BLOCKS)?;
    let mut large = Bench::new(KEYSTROKE_BLOCKS)?;
    for round in 0..WARMUP + SAMPLES {
        let keep = round >= WARMUP;
        small.step(key, keep)?;
        large.step(key, keep)?;
    }
    Ok((small.stats(), large.stats()))
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
