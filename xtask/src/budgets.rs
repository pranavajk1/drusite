// SPDX-License-Identifier: AGPL-3.0-or-later

//! The budgets, and the report every check writes into.
//!
//! Changing a budget is a deliberate, reviewed change to this file, never a CI setting.

use std::fmt::Write as _;
use std::io::Write as _;

use anyhow::Result;

/// Idle RAM of the desktop app, summed over all its processes (the webview's included), with a
/// 1,000-page workspace and one page open. From the architecture's "Efficiency and scale".
pub(crate) const RAM_IDLE_TOTAL_BYTES: u64 = 250 * MIB;

/// Pages in the workspace the RAM check opens.
pub(crate) const RAM_FIXTURE_PAGES: usize = 1_000;

/// Blocks in the page the keystroke check types into.
pub(crate) const KEYSTROKE_BLOCKS: usize = 10_000;

/// The 95th-percentile cost of one keystroke in the core: apply it to the page's Loro document,
/// commit, and export the update the store will append. Spike 1 measured ProseMirror alone at
/// 4.7 ms a keystroke at 10,000 blocks on a phone, against an 8.3 ms frame at 120 Hz, so the
/// core's share has to stay a small part of the rest. Loro alone costs a few µs on a Mac; the
/// headroom is for what later steps add to the same path (the store append, the block patch).
pub(crate) const KEYSTROKE_P95_NANOS: u64 = 500_000;

/// The median keystroke at [`KEYSTROKE_BLOCKS`] may cost at most this many times the median
/// keystroke in a 100-block page. This is what failed in spike 1: the stock binding re-walked the
/// whole page, so its cost grew with page size. A ratio holds on fast and slow runners alike.
pub(crate) const KEYSTROKE_SCALING_MAX: f64 = 3.0;

pub(crate) const MIB: u64 = 1024 * 1024;

/// One measured value against its limit.
#[derive(Debug)]
struct Line {
    name: String,
    measured: String,
    limit: String,
    ok: bool,
}

/// Collects every check's results, prints them, and writes them to the GitHub Actions job summary
/// when there is one.
#[derive(Debug, Default)]
pub(crate) struct Report {
    lines: Vec<Line>,
    notes: Vec<String>,
}

impl Report {
    pub(crate) fn record(
        &mut self,
        name: impl Into<String>,
        measured: impl Into<String>,
        limit: impl Into<String>,
        ok: bool,
    ) {
        let line = Line {
            name: name.into(),
            measured: measured.into(),
            limit: limit.into(),
            ok,
        };
        println!(
            "{} {}: {} (budget {})",
            if line.ok { "PASS" } else { "FAIL" },
            line.name,
            line.measured,
            line.limit
        );
        self.lines.push(line);
    }

    /// Context that belongs with the results, such as what a stub measured.
    pub(crate) fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        println!("note: {note}");
        self.notes.push(note);
    }

    /// Writes the job summary and returns whether every budget held.
    pub(crate) fn finish(self) -> Result<bool> {
        let ok = self.lines.iter().all(|l| l.ok);
        if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
            let mut md = String::from(
                "### Performance budgets\n\n| | check | measured | budget |\n|---|---|---|---|\n",
            );
            for l in &self.lines {
                let mark = if l.ok { "✅" } else { "❌" };
                let _ = writeln!(md, "| {mark} | {} | {} | {} |", l.name, l.measured, l.limit);
            }
            for n in &self.notes {
                let _ = writeln!(md, "\n{n}");
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?
                .write_all(md.as_bytes())?;
        }
        Ok(ok)
    }
}

pub(crate) fn fmt_nanos(nanos: u64) -> String {
    if nanos >= 1_000_000 {
        format!("{:.2} ms", nanos as f64 / 1e6)
    } else {
        format!("{:.1} µs", nanos as f64 / 1e3)
    }
}

pub(crate) fn fmt_bytes(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / MIB as f64)
}
