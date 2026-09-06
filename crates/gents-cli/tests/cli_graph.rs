//! Checkout-independent binary acceptance for bundled graph discovery and
//! revision-backed installation. Runtime/model execution has its own live
//! fixture; these cases keep the package boundary honest in the ordinary CLI.

mod support;

use anyhow::{Context, Result};
use serde_json::Value;

use support::{
    agent_did_from_init, allocate_port, run_cli_failure_stderr, run_cli_json, run_init_json,
    spawn_server_with_ready_json,
};

fn required_str<'a>(value: &'a Value, path: &[&str]) -> Result<&'a str> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .with_context(|| format!("missing JSON path {} in {value}", path.join(".")))?;
    }
    current
        .as_str()
        .with_context(|| format!("JSON path {} is not a string", path.join(".")))
}

#[test]
fn all_pack_kinds_are_available_without_a_checkout() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let catalog = run_cli_json(temp.path(), &["pack", "list"])?;
    anyhow::ensure!(catalog["packs"].as_array().context("packs")?.len() >= 11);
    for name in ["code_review", "pipeline", "mailbox", "graph_pipeline"] {
        let shown = run_cli_json(temp.path(), &["pack", "show", name])?;
        anyhow::ensure!(shown["manifest"]["name"] == name);
    }
    let root = temp.path().join("assets");
    let root_arg = root.to_str().context("path")?;
    let denial = run_cli_failure_stderr(
        temp.path(),
        &[
            "pack", "install", "mailbox", "--home", root_arg, "--output", "text",
        ],
    )?;
    anyhow::ensure!(denial.contains("unsupported --output text"), "{denial}");
    anyhow::ensure!(!root.exists(), "invalid output format wrote pack assets");
    for flag in ["--force-rebind-concrete-did", "--agent-did"] {
        let mut invalid = vec!["pack", "install", "mailbox", "--home", root_arg, flag];
        if flag == "--agent-did" {
            invalid.push("did:key:unused");
        }
        let denial = run_cli_failure_stderr(temp.path(), &invalid)?;
        anyhow::ensure!(denial.contains("binding flags do not apply"), "{denial}");
        anyhow::ensure!(!root.exists(), "invalid options wrote pack assets");
    }
    let args = ["pack", "install", "mailbox", "--home", root_arg];
    let first = run_cli_json(temp.path(), &args)?;
    anyhow::ensure!(run_cli_json(temp.path(), &args)? == first);
    let installed = std::path::Path::new(required_str(&first, &["installed_assets"])?);
    anyhow::ensure!(installed
        .join("datastore_tool_surfaces/mailbox_writes/object.json")
        .is_file());
    std::fs::write(installed.join("README.md"), "operator edit")?;
    let denial = run_cli_failure_stderr(temp.path(), &args)?;
    anyhow::ensure!(denial.contains("installed asset was modified"));
    Ok(())
}

#[test]
fn document_pack_installs_without_seeding_and_is_idempotent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("node");
    let home_arg = home.to_str().context("path")?;
    run_init_json(
        temp.path(),
        &["--agent-name", "pack-installer", "--home", home_arg],
    )?;
    let args = [
        "pack",
        "install",
        "pipeline",
        "--home",
        home_arg,
        "--force-rebind-concrete-did",
    ];
    let first = run_cli_json(temp.path(), &args)?;
    anyhow::ensure!(first["ok"] == true, "{first}");
    let second = run_cli_json(temp.path(), &args)?;
    anyhow::ensure!(
        second["ok"] == true && second["changed"] == false,
        "{second}"
    );
    Ok(())
}

#[test]
fn bundled_catalog_is_read_only_outside_a_source_checkout() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating graph catalog tempdir")?;
    let catalog = run_cli_json(tempdir.path(), &["pack", "show", "code_review"])?;
    let packages = [catalog["graph"].clone()];
    anyhow::ensure!(packages.len() == 1, "unexpected catalog output: {catalog}");
    anyhow::ensure!(
        packages[0].get("name").and_then(Value::as_str) == Some("code_review"),
        "catalog did not return code_review: {catalog}"
    );
    anyhow::ensure!(
        std::fs::read_dir(tempdir.path())?.next().is_none(),
        "read-only catalog created files in a clean working directory"
    );
    Ok(())
}

#[test]
fn web_deep_research_is_in_the_bundled_catalog() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating graph catalog tempdir")?;
    let catalog = run_cli_json(tempdir.path(), &["pack", "show", "web_deep_research"])?;
    let packages = [catalog["graph"].clone()];
    anyhow::ensure!(packages.len() == 1, "unexpected catalog output: {catalog}");
    anyhow::ensure!(
        packages[0].get("name").and_then(Value::as_str) == Some("web_deep_research"),
        "catalog did not return web-deep-research: {catalog}"
    );
    anyhow::ensure!(
        packages[0]
            .get("entries")
            .and_then(Value::as_array)
            .is_some_and(|entries| entries
                .iter()
                .any(|entry| { entry.get("name").and_then(Value::as_str) == Some("research") })),
        "catalog package did not expose the research entry: {catalog}"
    );
    let dependencies = packages[0]
        .get("external_dependencies")
        .and_then(Value::as_array)
        .context("catalog package did not expose external dependencies")?;
    anyhow::ensure!(
        dependencies.len() == 1
            && dependencies[0].get("service_id").and_then(Value::as_str)
                == Some("web-research-mcp")
            && dependencies[0]
                .get("install_command")
                .and_then(Value::as_str)
                == Some("./scripts/stack install-mcp"),
        "catalog package exposed the wrong external dependency: {catalog}"
    );
    Ok(())
}

#[test]
fn clean_binary_install_is_idempotent_activates_and_is_owner_fenced() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating graph install tempdir")?;
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().context("graph home path is not UTF-8")?;
    let initialized = run_init_json(
        tempdir.path(),
        &["--agent-name", "graph-reviewer", "--home", home_arg],
    )?;
    let owner_did = agent_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    anyhow::ensure!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "server did not become ready: {readiness}"
    );

    let install_args = [
        "pack",
        "install",
        "code_review",
        "--home",
        home_arg,
        "--output",
        "json",
    ];
    let first = run_cli_json(tempdir.path(), &install_args)?;
    let second = run_cli_json(tempdir.path(), &install_args)?;
    anyhow::ensure!(
        first.get("install") == second.get("install"),
        "repeated install changed its durable receipt\nfirst: {first}\nsecond: {second}"
    );
    let revision = required_str(&first, &["install", "revision_digest"])?;
    anyhow::ensure!(
        required_str(&first, &["activation", "active_digest"])? == revision,
        "install did not activate its exact immutable revision: {first}"
    );

    let wrong_actor = "did:key:z6MkvGraphPackageIntruder";
    let denial = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "pack",
            "install",
            "code_review",
            "--home",
            home_arg,
            "--agent-did",
            wrong_actor,
        ],
    )?;
    anyhow::ensure!(
        denial.contains("package owner principal is missing"),
        "wrong-owner install did not fail at the identity boundary: {denial}"
    );

    let disabled = run_cli_json(
        tempdir.path(),
        &[
            "graph",
            "disable",
            "code_review",
            "--home",
            home_arg,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(disabled.get("enabled") == Some(&Value::Bool(false)));
    let enabled = run_cli_json(
        tempdir.path(),
        &[
            "graph",
            "enable",
            "code_review",
            "--home",
            home_arg,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(enabled.get("enabled") == Some(&Value::Bool(true)));
    Ok(())
}
