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

#[test]
fn bare_gents_without_a_terminal_prints_help_and_exits_2() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;

    for args in [&[][..], &["--home", &*home.path().to_string_lossy()][..]] {
        let output = run_gents(user_home.path(), args)?;

        let stderr = stderr_of(&output);
        assert_eq!(output.status.code(), Some(2), "{args:?} stderr:\n{stderr}");
        assert!(stderr.contains("\n  utils "), "{args:?} stderr:\n{stderr}");
        assert!(output.stdout.is_empty(), "{args:?} wrote to stdout");
        assert_eq!(
            fs::read_dir(user_home.path())?.count(),
            0,
            "{args:?} wrote to HOME"
        );
        assert_eq!(
            fs::read_dir(home.path())?.count(),
            0,
            "{args:?} wrote to the home"
        );
    }
    Ok(())
}

#[test]
fn root_flags_conflict_with_subcommands() -> Result<()> {
    let user_home = tempfile::tempdir()?;

    let output = run_gents(user_home.path(), &["--home", "X", "status"])?;

    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(stderr.contains("cannot be used with"), "stderr:\n{stderr}");
    Ok(())
}

#[test]
fn bare_gents_with_agent_and_no_terminal_exits_2() -> Result<()> {
    let user_home = tempfile::tempdir()?;

    let output = run_gents(user_home.path(), &["--agent", "X"])?;

    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(stderr.contains("\n  utils "), "stderr:\n{stderr}");
    assert_eq!(fs::read_dir(user_home.path())?.count(), 0, "wrote to HOME");
    Ok(())
}

#[test]
fn the_root_agent_flag_conflicts_with_subcommands() -> Result<()> {
    let user_home = tempfile::tempdir()?;

    let output = run_gents(user_home.path(), &["--agent", "X", "status"])?;

    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(stderr.contains("cannot be used with"), "stderr:\n{stderr}");
    Ok(())
}

fn init_home(user_home: &Path, home: &Path) -> Result<()> {
    let output = run_gents(
        user_home,
        &[
            "init",
            "--home",
            &home.to_string_lossy(),
            "--store-key-custody",
            "file",
            "--inference-url",
            "http://127.0.0.1:9/v1",
            "--model-name",
            "m",
        ],
    )?;
    anyhow::ensure!(
        output.status.success(),
        "gents init failed:\n{}",
        stderr_of(&output)
    );
    Ok(())
}

#[test]
fn config_client_set_and_show() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    init_home(user_home.path(), home.path())?;
    let home_arg = home.path().to_string_lossy();
    let show = || -> Result<String> {
        let output = run_gents(
            user_home.path(),
            &["config", "client", "show", "--home", &home_arg],
        )?;
        assert!(output.status.success(), "stderr:\n{}", stderr_of(&output));
        Ok(String::from_utf8(output.stdout)?)
    };
    assert_eq!(show()?.trim(), "codex");

    let set = run_gents(
        user_home.path(),
        &["config", "client", "set", "grok", "--home", &home_arg],
    )?;

    let stderr = stderr_of(&set);
    assert!(set.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("gents runtime service restart"),
        "stderr:\n{stderr}"
    );
    assert_eq!(show()?.trim(), "grok");
    assert!(
        !home.path().join("runtime.json").exists(),
        "setting the client must not start a runtime"
    );
    Ok(())
}

#[test]
fn config_client_refuses_an_uninitialized_home() -> Result<()> {
    let user_home = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let home_arg = home.path().to_string_lossy();

    for args in [
        &["config", "client", "set", "grok", "--home", &home_arg][..],
        &["config", "client", "show", "--home", &home_arg][..],
    ] {
        let output = run_gents(user_home.path(), args)?;

        let stderr = stderr_of(&output);
        assert!(!output.status.success(), "{args:?} stderr:\n{stderr}");
        assert!(
            stderr.contains(&format!("gents --home {home_arg}")),
            "{args:?} stderr:\n{stderr}"
        );
    }
    assert!(!home.path().join("client.json").exists());
    Ok(())
}
