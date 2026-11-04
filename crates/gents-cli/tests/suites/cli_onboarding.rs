use crate::support::*;

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `gents` with `user_home` as `HOME`, so the remembered file and the default
/// home both resolve under it.
fn run_gents(user_home: &Path, args: &[&str]) -> Result<Output> {
    Command::new(cli_bin())
        .env("HOME", user_home)
        .env_remove("GENTS_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env("RUST_LOG", "error")
        .args(args)
        .output()
        .with_context(|| format!("running gents {}", args.join(" ")))
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn remembered_home_path(user_home: &Path) -> PathBuf {
    let config_dir = if cfg!(target_os = "macos") {
        user_home.join("Library/Application Support")
    } else {
        user_home.join(".config")
    };
    config_dir.join("gents/home")
}

#[test]
fn no_flag_with_an_unreadable_remembered_file_names_it() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let remembered = remembered_home_path(user_home.path());
    fs::create_dir_all(&remembered)?;

    let output = run_gents(user_home.path(), &["status"])?;

    let stderr = stderr_of(&output);
    assert!(!output.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains(&*remembered.to_string_lossy()),
        "expected {} in stderr, got:\n{stderr}",
        remembered.display()
    );
    Ok(())
}

#[test]
fn explicit_home_never_reads_the_remembered_file() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let remembered = remembered_home_path(user_home.path());
    fs::create_dir_all(&remembered)?;

    let output = run_gents(
        user_home.path(),
        &["status", "--home", &home.path().to_string_lossy()],
    )?;

    let stderr = stderr_of(&output);
    assert!(
        !stderr.contains(&*remembered.to_string_lossy()),
        "stderr mentions the remembered file:\n{stderr}"
    );
    assert!(
        stderr.contains(&*home.path().to_string_lossy()),
        "expected the status failure for the explicit home, got:\n{stderr}"
    );
    Ok(())
}

#[test]
fn explicit_home_never_changes_the_remembered_file() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let remembered = remembered_home_path(user_home.path());
    fs::create_dir_all(
        remembered
            .parent()
            .context("remembered file has a parent")?,
    )?;
    let planted = b"/srv/remembered home\n";
    fs::write(&remembered, planted)?;

    run_gents(
        user_home.path(),
        &["status", "--home", &home.path().to_string_lossy()],
    )?;

    assert_eq!(fs::read(&remembered)?, planted);
    Ok(())
}
