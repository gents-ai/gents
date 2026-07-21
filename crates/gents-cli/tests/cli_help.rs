mod support;
use support::*;

use anyhow::{Context, Result};
use std::fs;

#[test]
fn top_level_help_shows_quickstart_workflow() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = run_cli_text(&home_dir, &["--help"])?;
    assert!(
        output.contains("Quick start:"),
        "expected quick start section in help output, got:\n{output}"
    );
    assert!(
        output.contains("gents init"),
        "expected init example in help output, got:\n{output}"
    );
    assert!(
        output.contains("gents server"),
        "expected server example in help output, got:\n{output}"
    );
    assert!(
        output.contains("gents chat"),
        "expected chat example in help output, got:\n{output}"
    );

    Ok(())
}

#[test]
fn status_without_runtime_suggests_init_and_server() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);

    let stderr = run_cli_failure_stderr(&home_dir, &["status", "--graphql", &graphql])?;
    assert!(
        stderr.contains("gents init"),
        "expected init suggestion in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("gents server"),
        "expected server suggestion in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("gents status"),
        "expected status suggestion in stderr, got:\n{stderr}"
    );

    Ok(())
}

#[test]
fn server_help_does_not_suggest_unspecified_codex_shim_bind() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = run_cli_text(&home_dir, &["server", "--help"])?;
    assert!(
        output.contains("<trusted-private-or-tailscale-ip>"),
        "expected trusted private bind guidance in server help, got:\n{output}"
    );
    assert!(
        !output.contains("0.0.0.0"),
        "server help must not suggest an unspecified unauthenticated Codex shim bind, got:\n{output}"
    );

    Ok(())
}
