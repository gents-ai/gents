use crate::support::*;

use anyhow::{Context, Result};
use std::fs;

#[test]
fn top_level_help_is_sectioned() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = run_cli_text(&home_dir, &["--help"])?;
    assert!(
        output.contains("Start               chat  codex  status"),
        "expected the Start section in help output, got:\n{output}"
    );
    assert!(
        output.contains("Data and debugging  data  debug"),
        "expected the Data and debugging section in help output, got:\n{output}"
    );
    assert!(
        !output.contains("Quick start"),
        "the quick start section should be gone, got:\n{output}"
    );

    Ok(())
}

#[test]
fn group_help_lists_its_own_subcommands() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = run_cli_text(&home_dir, &["auth", "--help"])?;
    for subcommand in ["login", "probe", "accounts"] {
        assert!(
            output.contains(subcommand),
            "expected {subcommand} in auth help output, got:\n{output}"
        );
    }

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
fn server_help_requires_authentication_for_remote_app_server_bind() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = run_cli_text(&home_dir, &["server", "--help"])?;
    assert!(
        output.contains("--codex-shim-auth-token-env"),
        "expected authenticated remote-bind guidance in server help, got:\n{output}"
    );
    assert!(
        !output.contains("0.0.0.0"),
        "server help must not suggest an unspecified unauthenticated Codex shim bind, got:\n{output}"
    );

    Ok(())
}
