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
        output.contains("defra-agent init"),
        "expected init example in help output, got:\n{output}"
    );
    assert!(
        output.contains("defra-agent server"),
        "expected server example in help output, got:\n{output}"
    );
    assert!(
        output.contains("defra-agent chat"),
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
        stderr.contains("defra-agent init"),
        "expected init suggestion in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("defra-agent server"),
        "expected server suggestion in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("defra-agent status"),
        "expected status suggestion in stderr, got:\n{stderr}"
    );

    Ok(())
}
