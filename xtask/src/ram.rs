// SPDX-License-Identifier: AGPL-3.0-or-later

//! The idle-RAM budget: start the desktop app on a 1,000-page workspace with one page open, let
//! it go idle, and add up the memory of every process that belongs to it.
//!
//! The contract with the app (see `crates/drusite-app`):
//! - `DRUSITE_DATA_DIR`: the workspace to open.
//! - `DRUSITE_BENCH_READY_FILE`: the app creates it once it is idle with a page open.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::budgets::{RAM_FIXTURE_PAGES, RAM_IDLE_TOTAL_BYTES, Report, fmt_bytes};
use crate::mem;
use crate::spawn::App;

const READY_TIMEOUT: Duration = Duration::from_secs(120);
const SAMPLES: usize = 5;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) app: Option<PathBuf>,
    pub(crate) settle_secs: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            app: None,
            settle_secs: 10,
        }
    }
}

pub(crate) fn check(report: &mut Report, opts: &Options) -> Result<()> {
    let work = target_dir().join("budgets");
    let app = match &opts.app {
        Some(app) => app.clone(),
        None => build_app()?,
    };
    let data_dir = work.join(format!("workspace-{RAM_FIXTURE_PAGES}"));
    write_fixture(&data_dir)?;
    let ready_file = work.join("app-ready");
    let _ = std::fs::remove_file(&ready_file);

    println!("ram: starting {}", app.display());
    let mut child = App::spawn(
        &app,
        &[
            ("DRUSITE_DATA_DIR", data_dir.as_os_str()),
            ("DRUSITE_BENCH_READY_FILE", ready_file.as_os_str()),
        ],
    )?;

    wait_ready(&mut child, &ready_file)?;
    std::thread::sleep(Duration::from_secs(opts.settle_secs));

    let mut totals = Vec::with_capacity(SAMPLES);
    let mut last = Vec::new();
    for i in 0..SAMPLES {
        if i > 0 {
            std::thread::sleep(SAMPLE_INTERVAL);
        }
        if let Some(status) = child.exited()? {
            bail!("the app exited while being measured ({status})");
        }
        last = mem::app_processes(child.id())?;
        totals.push(last.iter().map(|p| p.bytes).sum::<u64>());
    }
    totals.sort_unstable();
    let median = totals[totals.len() / 2];

    for p in &last {
        println!(
            "ram:   {:>8}  {:>10}  {}",
            p.pid,
            fmt_bytes(p.bytes),
            p.name
        );
    }
    report.record(
        format!(
            "idle RAM, {} process(es), {RAM_FIXTURE_PAGES}-page workspace",
            last.len()
        ),
        fmt_bytes(median),
        format!("≤ {}", fmt_bytes(RAM_IDLE_TOTAL_BYTES)),
        median <= RAM_IDLE_TOTAL_BYTES,
    );
    if opts.app.is_none() {
        report.note(
            "ram: measures the drusite-app stub, which has no webview yet, on an empty workspace. \
             The fixture generator arrives with the local store (Phase 1 step 3) and the real \
             app with the editor (step 5).",
        );
    }
    Ok(())
}

/// Writes the workspace the app opens. Until the local store exists (Phase 1 step 3) there is
/// nothing to write but the folder.
fn write_fixture(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    Ok(())
}

fn build_app() -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--release", "--locked", "--package", "drusite-app"])
        .status()
        .context("cannot run cargo")?;
    if !status.success() {
        bail!("building drusite-app failed");
    }
    Ok(target_dir()
        .join("release")
        .join(format!("drusite-app{}", std::env::consts::EXE_SUFFIX)))
}

fn wait_ready(child: &mut App, ready_file: &Path) -> Result<()> {
    let start = Instant::now();
    while !ready_file.exists() {
        if let Some(status) = child.exited()? {
            bail!("the app exited before it was ready ({status})");
        }
        if start.elapsed() > READY_TIMEOUT {
            bail!("the app wasn't ready after {}s", READY_TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("ram: ready after {:.1}s", start.elapsed().as_secs_f64());
    Ok(())
}

fn target_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return dir.into();
    }
    // xtask lives one level below the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
}
