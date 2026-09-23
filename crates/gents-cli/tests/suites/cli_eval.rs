use crate::support::*;

use anyhow::{Context, Result};

#[test]
fn eval_run_help_and_a_missing_definition_refuses_before_any_run_directory_exists() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    std::fs::create_dir_all(&home_dir)?;

    let help = run_cli_text(&home_dir, &["eval", "run", "--help"])?;
    assert!(help.contains("--cell"), "{help}");

    run_init_json(&home_dir, &[])?;
    let packs = run_cli_json(&home_dir, &["pack", "list"])?;
    let pack = packs["packs"][0]["name"]
        .as_str()
        .context("a bundled pack name")?
        .to_owned();
    let cell = format!("baseline={pack}:monitor");
    let output = std::process::Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .args([
            "eval",
            "run",
            "missing-def",
            "--cell",
            &cell,
            "--profile",
            "baseline=local",
            "--run-id",
            "smoke",
        ])
        .output()
        .context("running gents")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "a refusal exits 1: {stderr}");
    assert!(
        stderr.contains("no eval definition \"missing-def\""),
        "{stderr}"
    );
    assert!(
        !home_dir.join(".gents/eval/runs/smoke").exists(),
        "refused before any run directory or trial home is created"
    );
    Ok(())
}

/// An argv-only mistake spanning flags is a usage error: exit 2, before any
/// home is read (this home was never initialized).
#[test]
fn a_repeated_cell_or_a_stray_or_repeated_profile_exits_2_as_a_usage_error() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let cases: [(&[&str], &str); 3] = [
        (
            &[
                "eval",
                "run",
                "x",
                "--cell",
                "a=p",
                "--profile",
                "other=local",
            ],
            "--profile names cell \"other\", which no --cell declares",
        ),
        (
            &[
                "eval",
                "run",
                "x",
                "--cell",
                "a=p",
                "--profile",
                "a=x",
                "--profile",
                "a=y",
            ],
            "--profile names cell \"a\" more than once",
        ),
        (
            &["eval", "run", "x", "--cell", "a=p", "--cell", "a=q"],
            "--cell declares cell \"a\" more than once",
        ),
    ];
    for (argv, message) in cases {
        let output = std::process::Command::new(cli_bin())
            .env("HOME", tempdir.path())
            .env("RUST_LOG", "error")
            .args(argv)
            .output()
            .context("running gents")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains(message), "{stderr}");
    }
    Ok(())
}

/// The cancel marker needs no database: it is written before the home is
/// opened, so a home that cannot be opened (here, never initialized; in use,
/// an embedded node the hosting process holds) still gets it, exit 0.
#[test]
fn eval_cancel_writes_the_marker_before_opening_the_home() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let run_dir = tempdir.path().join(".gents/eval/runs/r1");
    std::fs::create_dir_all(&run_dir)?;
    let output = std::process::Command::new(cli_bin())
        .env("HOME", tempdir.path())
        .env("RUST_LOG", "error")
        .args(["eval", "cancel", "r1"])
        .output()
        .context("running gents")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(run_dir.join("cancel").exists(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("no process is running run r1 now"),
        "{stdout}"
    );
    Ok(())
}
