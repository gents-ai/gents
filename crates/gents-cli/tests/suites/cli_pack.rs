use crate::support::{self, *};

use anyhow::{Context, Result};
use serde_json::Value;

/// A documents pack that ships a plugin: install stores the plugin and
/// records it, and remove takes back exactly what the install created. Every
/// pack here is scaffolded on the fly, so this needs no fixture at all.
#[test]
fn a_document_pack_with_a_plugin_installs_and_removes_completely() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("node");
    let home_arg = home.to_str().context("path")?;
    run_init_json(
        temp.path(),
        &["--node-name", "pack-remover", "--home", home_arg],
    )?;

    let gents = |args: &[&str]| -> Result<std::process::Output> {
        Ok(std::process::Command::new(support::cli_bin())
            .env("HOME", temp.path())
            .env("RUST_LOG", "error")
            .current_dir(temp.path())
            .args(args)
            .output()?)
    };
    for args in [
        &["pack", "new", "demo_tools"][..],
        &["pack", "add", "plugin", "echo_tool", "--dir", "demo_tools"],
        &["pack", "build", "demo_tools", "--out", "demo_tools.pack"],
    ] {
        let output = gents(args)?;
        anyhow::ensure!(
            output.status.success(),
            "gents {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let install = run_cli_json(
        temp.path(),
        &[
            "pack",
            "install",
            temp.path()
                .join("demo_tools.pack")
                .to_str()
                .context("path")?,
            "--home",
            home_arg,
        ],
    )?;
    anyhow::ensure!(
        install["apply"]["plugins"][0]["name"] == "echo_tool"
            && !install["apply"]["created"]
                .as_array()
                .context("created")?
                .is_empty(),
        "{install}"
    );
    let echoed = run_cli_json(
        temp.path(),
        &[
            "plugin",
            "run",
            "gents/echo_tool",
            "--home",
            home_arg,
            "--input",
            r#"{"a":1}"#,
        ],
    )?;
    anyhow::ensure!(echoed == serde_json::json!({"a": 1}), "{echoed}");

    let removed = run_cli_json(
        temp.path(),
        &["pack", "remove", "demo_tools", "--home", home_arg],
    )?;
    let sorted = |value: &Value| -> Result<Vec<String>> {
        let mut names: Vec<String> = serde_json::from_value(value.clone())?;
        names.sort();
        Ok(names)
    };
    anyhow::ensure!(
        sorted(&removed["removed"]["removed"])? == sorted(&install["apply"]["created"])?,
        "{removed}"
    );
    let gone = run_cli_failure_stderr(
        temp.path(),
        &[
            "plugin",
            "run",
            "gents/echo_tool",
            "--home",
            home_arg,
            "--input",
            "{}",
        ],
    )?;
    anyhow::ensure!(gone.contains("not installed"), "{gone}");
    let again = run_cli_failure_stderr(
        temp.path(),
        &["pack", "remove", "demo_tools", "--home", home_arg],
    )?;
    anyhow::ensure!(again.contains("is not installed"), "{again}");
    Ok(())
}
