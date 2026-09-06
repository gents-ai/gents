//! Live qualification: `gents pack run security_scan` end to end against
//! this repository, on the pack's default GLM-5.2 backend (or whatever
//! GENTS_SCAN_ENDPOINT / GENTS_SCAN_MODEL point at).
//!
//! ```bash
//! GENTS_LIVE_SECSCAN=1 cargo test -p gents-cli --test cli_pack_secscan_live \
//!   -- --ignored --test-threads=1 --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

#[test]
#[ignore]
fn pack_run_security_scan_live() {
    if std::env::var("GENTS_LIVE_SECSCAN").as_deref() != Ok("1") {
        eprintln!("GENTS_LIVE_SECSCAN != 1; skipping");
        return;
    }
    let root = repo_root();
    let status = Command::new(env!("CARGO_BIN_EXE_gents"))
        .current_dir(&root)
        .env("GENTS_SCAN_ROOT", &root)
        .args(["pack", "run", "security_scan"])
        .status()
        .expect("spawn gents pack run");
    assert!(status.success(), "pack run security_scan exited {status}");

    // The runner writes runs/<job_id>/meta.json; the newest run must exist
    // and record a results artifact.
    let runs = root.join("packs/security_scan/runs");
    let newest = std::fs::read_dir(&runs)
        .expect("runs dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .expect("at least one run dir");
    let meta = std::fs::read_to_string(newest.path().join("meta.json")).expect("meta.json");
    assert!(
        meta.contains("scan-report"),
        "meta.json missing final stage: {meta}"
    );
}
