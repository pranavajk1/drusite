// SPDX-License-Identifier: AGPL-3.0-or-later

//! Drusite's repo tooling. Run it with `cargo xtask <command>`.
//!
//! - `budgets`: the performance budgets CI enforces. Each check exits non-zero when its budget
//!   is broken, which fails the build.
//! - `bindings`: the UI's TypeScript types for the Core API.

mod bindings;
mod budgets;
mod keystroke;
mod mem;
mod ram;
mod spawn;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

const USAGE: &str = "\
usage: cargo xtask budgets [all|keystroke|ram] [options]
       cargo xtask bindings [--check]

bindings       writes the UI's TypeScript types for the Core API (--check: fail if stale)

  keystroke    the cost of one keystroke in a 10,000-block page
  ram          idle RAM of the desktop app, all processes, 1,000-page workspace, one page open
  all          both (the default)

options for ram:
  --app <path>       the app binary to measure (default: builds and runs drusite-app)
  --settle <secs>    how long to let the app go idle after it reports ready (default: 10)";

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("\nbudget check failed: at least one budget is broken");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::from(2)
        }
    }
}

/// Returns whether every budget that ran is within its limit.
fn run() -> Result<bool> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("budgets") => {}
        Some("bindings") => {
            let check = match args.next().as_deref() {
                Some("--check") => true,
                None => false,
                Some(other) => bail!("unknown argument `{other}`\n\n{USAGE}"),
            };
            return bindings::run(check);
        }
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            return Ok(true);
        }
        Some(other) => bail!("unknown command `{other}`\n\n{USAGE}"),
    }

    let mut which = "all".to_owned();
    let mut ram_opts = ram::Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "all" | "keystroke" | "ram" => which = arg,
            "--app" => {
                ram_opts.app = Some(PathBuf::from(args.next().context("--app needs a path")?))
            }
            "--settle" => {
                let secs = args.next().context("--settle needs a number of seconds")?;
                ram_opts.settle_secs =
                    secs.parse().context("--settle needs a number of seconds")?;
            }
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }

    let mut report = budgets::Report::default();
    if which == "all" || which == "keystroke" {
        keystroke::check(&mut report)?;
    }
    if which == "all" || which == "ram" {
        ram::check(&mut report, &ram_opts)?;
    }
    report.finish()
}
