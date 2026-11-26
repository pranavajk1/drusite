// SPDX-License-Identifier: AGPL-3.0-or-later

//! The keystroke budget: what one typed character costs the core in a long page.
//!
//! The page is built the way Drusite's block model stores it (a movable list of block maps, each
//! with its text in a `LoroText`). Step 2 of Phase 1 points this at `drusite-crdt`'s own edit API,
//! so the budget follows the real code path; until then it drives Loro directly in that shape.

use std::hint::black_box;
use std::time::Instant;

use anyhow::{Context, Result};
use loro::{ExportMode, LoroDoc, LoroMap, LoroMovableList, LoroText};

use crate::budgets::{
    KEYSTROKE_BLOCKS, KEYSTROKE_P95_NANOS, KEYSTROKE_SCALING_MAX, Report, fmt_nanos,
};

const SMALL_PAGE_BLOCKS: usize = 100;
const WARMUP: usize = 200;
const SAMPLES: usize = 2_000;

pub(crate) fn check(report: &mut Report) -> Result<()> {
    let small = measure(SMALL_PAGE_BLOCKS)?;
    let large = measure(KEYSTROKE_BLOCKS)?;

    report.record(
        format!("keystroke p95, {KEYSTROKE_BLOCKS} blocks"),
        fmt_nanos(large.p95),
        format!("≤ {}", fmt_nanos(KEYSTROKE_P95_NANOS)),
        large.p95 <= KEYSTROKE_P95_NANOS,
    );
    let ratio = large.median as f64 / small.median.max(1) as f64;
    report.record(
        format!("keystroke median, {KEYSTROKE_BLOCKS} vs {SMALL_PAGE_BLOCKS} blocks"),
        format!(
            "{ratio:.2}× ({} vs {})",
            fmt_nanos(large.median),
            fmt_nanos(small.median)
        ),
        format!("≤ {KEYSTROKE_SCALING_MAX:.1}×"),
        ratio <= KEYSTROKE_SCALING_MAX,
    );
    report.note(format!(
        "keystroke: {SAMPLES} samples per page size; max {} at {KEYSTROKE_BLOCKS} blocks. \
         Measures Loro in the block model's shape until drusite-crdt lands (Phase 1 step 2).",
        fmt_nanos(large.max)
    ));
    Ok(())
}

#[derive(Debug)]
struct Stats {
    median: u64,
    p95: u64,
    max: u64,
}

fn measure(blocks: usize) -> Result<Stats> {
    let doc = LoroDoc::new();
    doc.set_peer_id(1)?;
    let list = doc.get_movable_list("blocks");
    for i in 0..blocks {
        let block = list.insert_container(i, LoroMap::new())?;
        block.insert("id", format!("block-{i:05}"))?;
        block.insert("type", if i % 10 == 0 { "heading" } else { "paragraph" })?;
        let text = block.insert_container("text", LoroText::new())?;
        text.insert(0, &sample_text(i))?;
    }
    doc.commit();

    let mut rng = fastrand::Rng::with_seed(0xD5_5173);
    let mut samples = Vec::with_capacity(SAMPLES);
    for round in 0..WARMUP + SAMPLES {
        let index = rng.usize(..blocks);
        let start = Instant::now();
        let update = keystroke(&doc, &list, index, &mut rng)?;
        let elapsed = start.elapsed();
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
        max: at(1.0),
    })
}

/// One typed character: find the block, insert into its text, commit, export the update.
fn keystroke(
    doc: &LoroDoc,
    list: &LoroMovableList,
    index: usize,
    rng: &mut fastrand::Rng,
) -> Result<Vec<u8>> {
    let since = doc.oplog_vv();
    let block = list
        .get(index)
        .context("block index out of range")?
        .into_container()
        .ok()
        .and_then(|c| c.into_map().ok())
        .context("block is not a map")?;
    let text = block
        .get("text")
        .context("block has no text")?
        .into_container()
        .ok()
        .and_then(|c| c.into_text().ok())
        .context("block text is not a LoroText")?;
    let pos = rng.usize(..=text.len_unicode());
    text.insert(pos, "x")?;
    doc.commit();
    Ok(doc.export(ExportMode::updates(&since))?)
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
