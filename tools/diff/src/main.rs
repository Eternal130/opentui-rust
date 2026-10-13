//! opentui-diff — the differential acceptance instrument.
//!
//! Runs one scenario on both sides and compares cell grids:
//!
//! * oracle: `bun js/driver.ts` driving the REAL @opentui 0.5.14
//!   (npm tarball, sandbox install)
//! * rust: `opentui-core` rendering the same scenario in-process
//!
//! Then it emits a cell-level report (+ SGR replay check of the rust
//! side).
//!
//! Usage: opentui-diff <scenario.json> [--refresh-oracle] [--keep-oracle]
//! Oracle JSONs cache under tools/diff/out/oracle/<name>.json; reports
//! land in tools/diff/out/reports/<name>.md.

use opentui_diff::{compare, scenario, solid, vt};

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The @opentui version the sandbox pins (package.json + bun.lock) —
/// the only version an oracle may be captured against (see the
/// resolution incident in tools/diff/README).
const PINNED_CORE: &str = "0.5.14";

/// FNV-1a 64 → 16 hex chars (mirrors `fnv1a` in js/driver.ts). A
/// non-cryptographic staleness guard, not integrity protection.
fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The fingerprint the CURRENT driver+scenario pair produces — what a
/// cached oracle must carry to be trusted.
fn live_fingerprint(root: &Path, scenario_path: &Path) -> Result<scenario::CaptureFingerprint> {
    let driver = root.join("tools/diff/js/driver.ts");
    let driver_bytes = std::fs::read(&driver).context("reading driver.ts for the fingerprint")?;
    let scenario_bytes =
        std::fs::read(scenario_path).context("reading scenario for the fingerprint")?;
    Ok(scenario::CaptureFingerprint {
        core: PINNED_CORE.to_owned(),
        driver: fnv1a_hex(&driver_bytes),
        scenario: fnv1a_hex(&scenario_bytes),
    })
}

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
    // solid-js must resolve its CLIENT build (see the script's doc)
    let patch = root.join("tools/diff/js/ensure-solid-client-build.sh");
    let _ = Command::new("bash")
        .arg(&patch)
        .current_dir(&root)
        .status()
        .context("running ensure-solid-client-build.sh (is bash on PATH?)")?;
    let status = Command::new("bun")
        .arg(driver)
        .arg(scenario_path)
        .arg(out_path)
        .current_dir(sandbox)
        // bun's install/cache dir must live under the invoking user's
        // HOME — a leaked BUN_INSTALL pointing at another user's root
        // breaks cache writes with a cryptic FileNotFound
        .env_remove("BUN_INSTALL")
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

    let scenario_path = scenario_path
        .canonicalize()
        .context("scenario path not found")?;
    let expected = live_fingerprint(&repo_root(), &scenario_path)?;
    let mut refreshed = false;
    if refresh_oracle || !oracle_path.exists() {
        run_oracle(&scenario_path, &oracle_path)?;
        refreshed = true;
    }

    let mut oracle_text = std::fs::read_to_string(&oracle_path)?;
    let mut oracle: scenario::Capture =
        serde_json::from_str(&oracle_text).context("oracle JSON parse")?;
    // Cache discipline: a cached oracle is only trustworthy while its
    // capture fingerprint matches the live driver + scenario (and the
    // pinned core version). A mismatch means the cache predates a
    // driver/scenario change — or was captured by a wrong-version
    // driver (the resolution incident). Self-heal: re-capture instead
    // of comparing against a stale artifact.
    if !refreshed && oracle.capture.as_ref() != Some(&expected) {
        let why = match &oracle.capture {
            None => "no fingerprint (pre-incident capture)".to_owned(),
            Some(c) => format!(
                "cached capture {{core={}, driver={}, scenario={}}} != live {{core={}, driver={}, scenario={}}}",
                c.core, c.driver, c.scenario, expected.core, expected.driver, expected.scenario
            ),
        };
        println!("oracle cache stale ({why}) — refreshing");
        run_oracle(&scenario_path, &oracle_path)?;
        oracle_text = std::fs::read_to_string(&oracle_path)?;
        oracle = serde_json::from_str(&oracle_text).context("oracle JSON parse (post-refresh)")?;
        if oracle.capture.as_ref() != Some(&expected) {
            anyhow::bail!(
                "refreshed oracle fingerprint {:?} still != live {:?} — driver/capture drift",
                oracle.capture,
                expected
            );
        }
    }

    let scenario = scenario::load(&scenario_path)?;
    let rust = if scenario.mode.as_deref() == Some("solid") {
        solid::render_solid(&scenario)?
    } else {
        scenario::render_rust(&scenario)?
    };

    // The terminal cursor is only meaningful where the scenario drives it
    // (focus/key/input steps — the input lane). Other scenarios' oracle
    // streams end with a teardown cursor-restore from the async-first
    // frame flow (an emission artifact, not markdown behavior): skip the
    // cursor compare there (documented in tools/diff/README).
    let cursor_relevant = scenario
        .steps
        .iter()
        .any(|s| matches!(s.op.as_str(), "focus" | "key" | "input"));
    let report = compare::compare_filtered(&oracle, &rust, cursor_relevant);
    let md = report.to_markdown(&name);
    let report_path = out_dir.join("reports").join(format!("{name}.md"));
    std::fs::write(&report_path, &md)?;
    println!("{md}");
    println!("report: {}", report_path.display());

    // secondary: SGR replay of the rust frame
    match vt::replay_matches(&rust) {
        Ok(()) => println!("vt-replay: OK (SGR stream paints the cell grid)"),
        Err(e) if e.starts_with("SKIP") => println!("vt-replay: {e}"),
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
