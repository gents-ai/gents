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
            .env_remove("CARGO_TARGET_DIR")
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

/// Unroutable: a connection to it fails immediately rather than timing out,
/// so a test that must never reach the network still runs fast if it
/// accidentally does.
const UNROUTABLE_REGISTRY: &str = "http://127.0.0.1:9";

fn dir_arg(path: &std::path::Path) -> &str {
    path.to_str().expect("fixture path is not UTF-8")
}

/// An initialized offline home whose documents dependency is already in the
/// pack store, so the dependent installs with no registry.
struct DependentHome {
    tempdir: tempfile::TempDir,
    home: std::path::PathBuf,
    install_args: Vec<String>,
}

impl DependentHome {
    fn new(node_name: &str) -> Result<Self> {
        let tempdir = tempfile::tempdir().context("creating pack install tempdir")?;
        let home = tempdir.path().join("agent-home");
        let home_arg = home.to_str().context("pack home path is not UTF-8")?;
        let initialized = run_init_json(
            tempdir.path(),
            &["--node-name", node_name, "--home", home_arg],
        )?;
        let owner_did = node_did_from_init(&initialized)?;
        run_cli_json(
            tempdir.path(),
            &[
                "pack",
                "fetch",
                dir_arg(&fixture_pack_dir("dependee_fixture")),
                "--store",
                "--home",
                home_arg,
            ],
        )?;
        let install_args = [
            "pack",
            "install",
            dir_arg(&fixture_pack_dir("dependent_fixture")),
            "--home",
            home_arg,
            "--registry",
            UNROUTABLE_REGISTRY,
            "--inference-slot",
            &format!("worker={owner_did}:default-profile"),
        ]
        .map(str::to_owned)
        .to_vec();
        Ok(Self {
            tempdir,
            home,
            install_args,
        })
    }

    fn home_arg(&self) -> &str {
        self.home.to_str().expect("pack home path is not UTF-8")
    }

    fn install_args(&self) -> Vec<&str> {
        self.install_args.iter().map(String::as_str).collect()
    }
}

/// A documents pack whose documents dependency installs into the same offline
/// home under the one store claim the pack install holds, while a second
/// holder of that home is still refused.
#[test]
fn offline_pack_install_with_a_documents_dependency_holds_one_store_claim() -> Result<()> {
    let setup = DependentHome::new("port-installer")?;
    let install_args = setup.install_args();

    let installed = run_cli_json(setup.tempdir.path(), &install_args)?;
    anyhow::ensure!(
        installed["dependencies"] == serde_json::json!(["fixture/dependee_fixture"]),
        "pack install did not report its documents dependency: {installed}"
    );
    let denial = run_cli_failure_stderr(
        setup.tempdir.path(),
        &[
            "pack",
            "remove",
            "fixture/dependee_fixture",
            "--home",
            setup.home_arg(),
        ],
    )?;
    anyhow::ensure!(
        denial.contains("fixture/dependent_fixture"),
        "the documents dependency was not installed: {denial}"
    );

    let _held = gents::home::lock_store(&setup.home, &gents::home::default_data_dir(&setup.home))?;
    let denial = run_cli_failure_stderr(setup.tempdir.path(), &install_args)?;
    anyhow::ensure!(
        denial.contains("another Gents runtime")
            && denial.contains(&format!("process {}", std::process::id())),
        "a second holder of the home was not refused: {denial}"
    );
    Ok(())
}

/// Removing a documents dependency directly is refused while its dependent is
/// installed; removing the dependent releases it.
#[test]
fn a_documents_dependency_is_released_with_its_dependent() -> Result<()> {
    let setup = DependentHome::new("dep-remover")?;
    run_cli_json(setup.tempdir.path(), &setup.install_args())?;

    let denial = run_cli_failure_stderr(
        setup.tempdir.path(),
        &[
            "pack",
            "remove",
            "fixture/dependee_fixture",
            "--home",
            setup.home_arg(),
        ],
    )?;
    anyhow::ensure!(denial.contains("fixture/dependent_fixture"), "{denial}");

    let removed = run_cli_json(
        setup.tempdir.path(),
        &[
            "pack",
            "remove",
            "fixture/dependent_fixture",
            "--home",
            setup.home_arg(),
        ],
    )?;
    anyhow::ensure!(
        removed["removed"]["dependencies"][0]["pack"] == "fixture/dependee_fixture",
        "{removed}"
    );
    let denial = run_cli_failure_stderr(
        setup.tempdir.path(),
        &[
            "pack",
            "remove",
            "fixture/dependee_fixture",
            "--home",
            setup.home_arg(),
        ],
    )?;
    anyhow::ensure!(denial.contains("is not installed"), "{denial}");
    Ok(())
}

/// An explicit install of the same coordinate survives removing the dependent
/// that also names it.
#[test]
fn an_explicitly_installed_documents_dependency_survives_its_dependent() -> Result<()> {
    let setup = DependentHome::new("dep-explicit")?;
    let owner_slot = setup
        .install_args
        .last()
        .context("the install arguments end with the slot binding")?
        .clone();
    run_cli_json(
        setup.tempdir.path(),
        &[
            "pack",
            "install",
            dir_arg(&fixture_pack_dir("dependee_fixture")),
            "--home",
            setup.home_arg(),
            "--inference-slot",
            &owner_slot,
        ],
    )?;
    run_cli_json(setup.tempdir.path(), &setup.install_args())?;
    run_cli_json(
        setup.tempdir.path(),
        &[
            "pack",
            "remove",
            "fixture/dependent_fixture",
            "--home",
            setup.home_arg(),
        ],
    )?;
    let removed = run_cli_json(
        setup.tempdir.path(),
        &[
            "pack",
            "remove",
            "fixture/dependee_fixture",
            "--home",
            setup.home_arg(),
        ],
    )?;
    anyhow::ensure!(
        removed["pack"] == "fixture/dependee_fixture",
        "an explicit install of the dependency must survive removing its dependent: {removed}"
    );
    Ok(())
}
