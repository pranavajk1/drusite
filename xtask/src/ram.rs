// SPDX-License-Identifier: AGPL-3.0-or-later

//! The idle-RAM budget: start the desktop app on a 1,000-page workspace with one page open, let
//! it go idle, and add up the memory of every process that belongs to it.
//!
//! The contract with the app (see `crates/drusite-app`):
//! - `DRUSITE_DATA_DIR`: the folder holding the store to open (`workspace.drusite`).
//! - `DRUSITE_BENCH_KEY_FILE`: the store key, as hex. Only for benchmarks: the app normally
//!   takes its key from the Keychain.
//! - `DRUSITE_BENCH_READY_FILE`: the app creates it once it is idle with a page open.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use drusite_core::StoreKey;
use drusite_core::store::{DocKind, STORE_FILE, Store};
use drusite_crdt::Workspace;
use drusite_model::{BlockKind, HeadingLevel};

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
    /// Extra environment for the app, for experiments.
    pub(crate) envs: Vec<(String, String)>,
    /// Report the figure without enforcing the budget.
    pub(crate) report_only: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            app: None,
            settle_secs: 10,
            envs: Vec::new(),
            report_only: false,
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
    let started = std::time::Instant::now();
    let key_file = write_fixture(&data_dir)?;
    println!(
        "ram: wrote the {RAM_FIXTURE_PAGES}-page workspace in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    let ready_file = work.join("app-ready");
    let _ = std::fs::remove_file(&ready_file);

    println!("ram: starting {}", app.display());
    let mut envs: Vec<(&str, &std::ffi::OsStr)> = vec![
        ("DRUSITE_DATA_DIR", data_dir.as_os_str()),
        ("DRUSITE_BENCH_READY_FILE", ready_file.as_os_str()),
        ("DRUSITE_BENCH_KEY_FILE", key_file.as_os_str()),
    ];
    for (k, v) in &opts.envs {
        envs.push((k.as_str(), std::ffi::OsStr::new(v.as_str())));
    }
    let mut child = App::spawn(&app, &envs)?;

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
        let total = last.iter().map(|p| p.bytes).sum::<u64>();
        println!(
            "ram: sample {}: {} in {} process(es)",
            i + 1,
            fmt_bytes(total),
            last.len()
        );
        totals.push(total);
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
    let with: Vec<String> = opts.envs.iter().map(|(k, v)| format!(" {k}={v}")).collect();
    let name = format!(
        "idle RAM{}{}, {} process(es), {RAM_FIXTURE_PAGES}-page workspace",
        if with.is_empty() { "" } else { " with" },
        with.concat(),
        last.len()
    );
    if opts.report_only {
        report.note(format!("{name}: {} (not enforced)", fmt_bytes(median)));
    } else {
        report.record(
            name,
            fmt_bytes(median),
            format!("≤ {}", fmt_bytes(RAM_IDLE_TOTAL_BYTES)),
            median <= RAM_IDLE_TOTAL_BYTES,
        );
    }
    if opts.app.is_none() {
        report.note(
            "ram: measures the app with the page tree and the first page open in the editor.",
        );
    }
    Ok(())
}

/// Writes the workspace the app opens: an encrypted store holding a workspace of
/// [`RAM_FIXTURE_PAGES`] pages, each a short day's notes, and the key that opens it.
fn write_fixture(dir: &Path) -> Result<PathBuf> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    let key = StoreKey::generate()?;
    let key_file = dir.join("bench.key");
    std::fs::write(&key_file, key.to_hex().as_bytes())?;
    let mut store = Store::open(&dir.join(STORE_FILE), &key)?;
    let mut ws = Workspace::create()?;
    let mut ws_at = store.create_workspace(&ws)?;
    let mut parent = None;
    for i in 0..RAM_FIXTURE_PAGES {
        // Ten top-level sections of a hundred pages each.
        let under = if i % 100 == 0 { None } else { parent };
        let mut page = ws.create_page(under, &format!("Page {i}"))?;
        if i % 100 == 0 {
            parent = Some(page.id());
        }
        let mut last = page.order()[0];
        page.insert_text(last, 0, &format!("Notes for page {i}"))?;
        for b in 0..20 {
            let kind = match b % 5 {
                0 => BlockKind::Heading {
                    level: HeadingLevel::H2,
                },
                1 => BlockKind::Bulleted,
                2 => BlockKind::Todo {
                    checked: b % 2 == 0,
                },
                _ => BlockKind::Paragraph,
            };
            let (id, _) = page.insert_block_after(Some(last), kind)?;
            page.insert_text(
                id,
                0,
                "The quick brown fox jumps over the lazy dog, again and again.",
            )?;
            last = id;
        }
        store.create_page(&page)?;
    }
    store.save_workspace(&ws, &mut ws_at)?;
    let pages = store.docs(DocKind::Page)?.len();
    anyhow::ensure!(
        pages == RAM_FIXTURE_PAGES,
        "the fixture holds {pages} pages"
    );
    Ok(key_file)
}

fn build_app() -> Result<PathBuf> {
    // The app embeds the built UI.
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui");
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    if !ui.join("node_modules").exists() {
        let status = Command::new(npm)
            .args(["ci", "--no-fund", "--no-audit"])
            .current_dir(&ui)
            .status()
            .context("cannot run npm")?;
        if !status.success() {
            bail!("npm ci failed");
        }
    }
    let status = Command::new(npm)
        .args(["run", "build"])
        .current_dir(&ui)
        .status()
        .context("cannot run npm")?;
    if !status.success() {
        bail!("building the UI failed");
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args([
            "build",
            "--release",
            "--locked",
            "--package",
            "drusite-app",
            "--features",
            "custom-protocol",
        ])
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
