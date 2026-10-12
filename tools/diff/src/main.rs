//! opentui-diff — the differential acceptance instrument.
//!
//! Runs one scenario on both sides and compares cell grids:
//!   * oracle: `bun js/driver.ts` driving the REAL @opentui 0.5.14
//!     (npm tarball, sandbox install)
//!   * rust: `opentui-core` rendering the same scenario in-process
//! then emits a cell-level report (+ SGR replay check of the rust side).
//!
//! Usage: opentui-diff <scenario.json> [--refresh-oracle] [--keep-oracle]
//! Oracle JSONs cache under tools/diff/out/oracle/<name>.json; reports
//! land in tools/diff/out/reports/<name>.md.

mod compare;
mod scenario;
mod vt;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/tools/diff → repo root is two levels up
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn run_oracle(scenario_path: &Path, out_path: &Path) -> Result<()> {
    let root = repo_root();
    let driver = root.join("tools/diff/js/driver.ts");
    let sandbox = root.join("tools/diff/sandbox");
    let status = Command::new("bun")
        .arg(driver)
        .arg(scenario_path)
        .arg(out_path)
        .current_dir(sandbox)
        .status()
        .context("spawning bun (is it on PATH?)")?;
    if !status.success() {
        anyhow::bail!("oracle driver failed: {status}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let scenario_path = PathBuf::from(
        args.next()
            .context("usage: opentui-diff <scenario.json> [--refresh-oracle]")?,
    );
    let refresh_oracle = args.any(|a| a == "--refresh-oracle");
    let name = scenario_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("scenario")
        .to_owned();

    let out_dir = repo_root().join("tools/diff/out");
    std::fs::create_dir_all(out_dir.join("oracle"))?;
    std::fs::create_dir_all(out_dir.join("reports"))?;
    let oracle_path = out_dir.join("oracle").join(format!("{name}.json"));

    let scenario_path = scenario_path.canonicalize().context("scenario path not found")?;
    if refresh_oracle || !oracle_path.exists() {
        run_oracle(&scenario_path, &oracle_path)?;
    }

    let oracle_text = std::fs::read_to_string(&oracle_path)?;
    let oracle: scenario::Capture =
        serde_json::from_str(&oracle_text).context("oracle JSON parse")?;
    let scenario = scenario::load(&scenario_path)?;
    let rust = scenario::render_rust(&scenario)?;

    let report = compare::compare(&oracle, &rust);
    let md = report.to_markdown(&name);
    let report_path = out_dir.join("reports").join(format!("{name}.md"));
    std::fs::write(&report_path, &md)?;
    println!("{md}");
    println!("report: {}", report_path.display());

    // secondary: SGR replay of the rust frame
    match vt::replay_matches(&rust) {
        Ok(()) => println!("vt-replay: OK (SGR stream paints the cell grid)"),
        Err(e) => println!("vt-replay: MISMATCH — {e}"),
    }

    if report.identical {
        println!("result: IDENTICAL vs @opentui 0.5.14");
        Ok(())
    } else {
        println!("result: MISMATCH vs @opentui 0.5.14");
        std::process::exit(1);
    }
}
