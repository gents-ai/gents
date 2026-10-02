//! Checkout-independent binary acceptance for pack resolution, install, and
//! revision-backed graph running. Every case resolves a fixture pack from
//! its own directory (never a name this binary would have to have compiled
//! in); an unroutable registry keeps an accidental network fallback from
//! hanging instead of failing fast. Runtime/model execution has its own live
//! fixture; these cases keep the package boundary honest in the ordinary CLI.

mod support;

use anyhow::{Context, Result};
use serde_json::Value;

use support::{
    agent_did_from_init, allocate_port, copy_dir_all, first_graphql_row, fixture_pack_dir,
    graphql_query, run_cli_failure_stderr, run_cli_json, run_cli_text, run_init_json,
    spawn_server_with_ready_json,
};

/// Unroutable: a connection to it fails immediately rather than timing out,
/// so a test that must never reach the network still runs fast if it
/// accidentally does.
const UNROUTABLE_REGISTRY: &str = "http://127.0.0.1:9";

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

fn dir_arg(path: &std::path::Path) -> &str {
    path.to_str().expect("fixture path is not UTF-8")
}

/// No pack ships inside this binary any more: an empty home lists nothing,
/// and a name it does not hold fails with the store/registry sentence
/// instead of a compiled-in catalog hit.
#[test]
fn the_binary_embeds_no_packs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let listed = run_cli_json(temp.path(), &["pack", "list"])?;
    anyhow::ensure!(listed["packs"] == serde_json::json!([]), "{listed}");
    let denial = run_cli_failure_stderr(
        temp.path(),
        &[
            "pack",
            "show",
            "code_review",
            "--registry",
            UNROUTABLE_REGISTRY,
        ],
    )?;
    anyhow::ensure!(
        denial.contains("is not in the pack store of") && denial.contains("could not be reached"),
        "{denial}"
    );
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
    let pack = fixture_pack_dir("documents_fixture");
    let args = [
        "pack",
        "install",
        dir_arg(&pack),
        "--home",
        home_arg,
        "--force-rebind-concrete-did",
    ];
    let first = run_cli_json(temp.path(), &args)?;
    anyhow::ensure!(first["apply"]["counts"]["AgentBehavior"] == 1, "{first}");
    let before_root = temp.path().join("before");
    run_cli_text(
        temp.path(),
        &[
            "config",
            "export",
            "--home",
            home_arg,
            "--root",
            before_root.to_str().context("export path")?,
        ],
    )?;
    let before = support::read_json_file(&before_root.join("pack_config.json"))?;
    let second = run_cli_json(temp.path(), &args)?;
    anyhow::ensure!(
        second["apply"]["counts"] == first["apply"]["counts"]
            && second["apply"]["created"] == serde_json::json!([])
            && second["apply"]["replaced"] == first["apply"]["created"]
            && second["digest"] == first["digest"],
        "{second}"
    );
    let after_root = temp.path().join("after");
    run_cli_text(
        temp.path(),
        &[
            "config",
            "export",
            "--home",
            home_arg,
            "--root",
            after_root.to_str().context("export path")?,
        ],
    )?;
    anyhow::ensure!(
        before == support::read_json_file(&after_root.join("pack_config.json"))?,
        "reinstall changed canonical configuration"
    );
    Ok(())
}

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
        &["--agent-name", "pack-remover", "--home", home_arg],
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

/// Every declared inference-slot install, activate, disable/enable cycle on
/// a graph pack resolved from its own directory: `review_graph` never
/// shipped a plan, so this also proves fresh compilation at install time.
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

    let profile = format!("{owner_did}:default-profile");
    let coordinator = format!("coordinator={profile}");
    let worker = format!("worker={profile}");
    let verifier = format!("verifier={profile}");
    let pack = fixture_pack_dir("review_graph");
    let install_args = [
        "pack",
        "install",
        dir_arg(&pack),
        "--home",
        home_arg,
        "--output",
        "json",
        "--inference-slot",
        &coordinator,
        "--inference-slot",
        &worker,
        "--inference-slot",
        &verifier,
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
            dir_arg(&pack),
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
            "fixture/review_graph",
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
            "fixture/review_graph",
            "--home",
            home_arg,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(enabled.get("enabled") == Some(&Value::Bool(true)));
    Ok(())
}

/// A documents pack whose graph dependency installs into the same offline
/// home under the one store claim the pack install holds, while a second
/// holder of that home is still refused. The dependency is pre-stored with
/// `pack fetch --store` first, so the whole install runs with no registry.
#[test]
fn offline_pack_install_with_a_graph_dependency_holds_one_store_claim() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating pack install tempdir")?;
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().context("pack home path is not UTF-8")?;
    let initialized = run_init_json(
        tempdir.path(),
        &["--agent-name", "port-installer", "--home", home_arg],
    )?;
    let owner_did = agent_did_from_init(&initialized)?;

    let review_graph = fixture_pack_dir("review_graph");
    run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "fetch",
            dir_arg(&review_graph),
            "--store",
            "--home",
            home_arg,
        ],
    )?;

    let profile = format!("{owner_did}:default-profile");
    let slots: Vec<String> = ["coordinator", "worker", "verifier"]
        .iter()
        .map(|slot| format!("{slot}={profile}"))
        .collect();
    let dependent = fixture_pack_dir("dependent_fixture");
    let mut install_args = vec![
        "pack".to_owned(),
        "install".to_owned(),
        dir_arg(&dependent).to_owned(),
        "--home".to_owned(),
        home_arg.to_owned(),
        "--registry".to_owned(),
        UNROUTABLE_REGISTRY.to_owned(),
    ];
    for slot in &slots {
        install_args.extend(["--inference-slot".to_owned(), slot.clone()]);
    }
    let install_args_ref: Vec<&str> = install_args.iter().map(String::as_str).collect();

    let installed = run_cli_json(tempdir.path(), &install_args_ref)?;
    anyhow::ensure!(
        installed["dependencies"] == serde_json::json!(["fixture/review_graph"])
            && installed["owner"] == owner_did.as_str(),
        "pack install did not report its graph dependency: {installed}"
    );
    let enabled = run_cli_json(
        tempdir.path(),
        &[
            "graph",
            "enable",
            "fixture/review_graph",
            "--home",
            home_arg,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(
        enabled.get("enabled") == Some(&Value::Bool(true)),
        "the graph dependency was not installed: {enabled}"
    );

    let _held = gents::home::lock_store(&home, &gents::home::default_data_dir(&home))?;
    let denial = run_cli_failure_stderr(tempdir.path(), &install_args_ref)?;
    anyhow::ensure!(
        denial.contains("another Gents runtime")
            && denial.contains(&format!("process {}", std::process::id())),
        "a second holder of the home was not refused: {denial}"
    );
    Ok(())
}

/// An assets pack installs and removes with no `gents init` ever run: no
/// node is opened for either operation.
#[test]
fn an_assets_pack_removes_completely_from_an_uninitialized_home() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("assets-home");
    let home_arg = home.to_str().context("path")?;
    let pack = fixture_pack_dir("assets_fixture");

    let install = run_cli_json(
        temp.path(),
        &["pack", "install", dir_arg(&pack), "--home", home_arg],
    )?;
    let installed_assets = std::path::Path::new(required_str(&install, &["installed_assets"])?);
    anyhow::ensure!(installed_assets.is_dir(), "{install}");
    anyhow::ensure!(
        installed_assets.join("data/nested.json").is_file(),
        "{install}"
    );
    anyhow::ensure!(
        !home.join("data").exists(),
        "an assets-only install never opens a node"
    );

    let removed = run_cli_json(
        temp.path(),
        &[
            "pack",
            "remove",
            "fixture/assets_fixture",
            "--home",
            home_arg,
        ],
    )?;
    anyhow::ensure!(
        removed["removed"]["assets"]
            .as_array()
            .context("assets")?
            .len()
            == 1,
        "{removed}"
    );
    anyhow::ensure!(!installed_assets.exists(), "the cache version was removed");
    anyhow::ensure!(
        !home.join("data").exists(),
        "removal never opened a node either"
    );

    let again = run_cli_failure_stderr(
        temp.path(),
        &[
            "pack",
            "remove",
            "fixture/assets_fixture",
            "--home",
            home_arg,
        ],
    )?;
    anyhow::ensure!(again.contains("is not installed"), "{again}");
    Ok(())
}

/// A graph pack removes completely (its `GraphDefinition`/`GraphRevision`
/// and derived triggers are gone) and reinstalls cleanly afterward.
#[test]
fn a_graph_pack_removes_completely_and_reinstalls() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating graph remove tempdir")?;
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().context("path")?;
    let initialized = run_init_json(
        tempdir.path(),
        &["--agent-name", "graph-remover", "--home", home_arg],
    )?;
    let owner_did = agent_did_from_init(&initialized)?;
    let profile = format!("{owner_did}:default-profile");
    let pack = fixture_pack_dir("review_graph");
    let install_args = [
        "pack",
        "install",
        dir_arg(&pack),
        "--home",
        home_arg,
        "--inference-slot",
        &format!("coordinator={profile}"),
        "--inference-slot",
        &format!("worker={profile}"),
        "--inference-slot",
        &format!("verifier={profile}"),
    ];
    run_cli_json(tempdir.path(), &install_args)?;

    let removed = run_cli_json(
        tempdir.path(),
        &["pack", "remove", "fixture/review_graph", "--home", home_arg],
    )?;
    let retained = removed["removed"]["retained"]
        .as_array()
        .context("retained")?;
    anyhow::ensure!(
        retained.iter().any(|entry| entry["item"]
            .as_str()
            .unwrap_or_default()
            .starts_with("schema ")),
        "the package's SDL schema must be reported retained, not silently dropped: {removed}"
    );

    let denial = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "graph",
            "enable",
            "fixture/review_graph",
            "--home",
            home_arg,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(
        denial.contains("package graph is not installed"),
        "{denial}"
    );

    run_cli_json(tempdir.path(), &install_args)?;
    Ok(())
}

/// A documents pack's graph dependency is tracked: removing the dependency
/// directly is refused while its dependent is installed; removing the
/// dependent releases it; an explicit install of the same coordinate
/// survives removing the dependent that also names it.
#[test]
fn a_graph_dependency_is_released_with_its_dependent() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating dependency remove tempdir")?;
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().context("path")?;
    let initialized = run_init_json(
        tempdir.path(),
        &["--agent-name", "dep-remover", "--home", home_arg],
    )?;
    let owner_did = agent_did_from_init(&initialized)?;
    let profile = format!("{owner_did}:default-profile");
    let slots: Vec<String> = ["coordinator", "worker", "verifier"]
        .iter()
        .map(|slot| format!("{slot}={profile}"))
        .collect();

    let review_graph = fixture_pack_dir("review_graph");
    run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "fetch",
            dir_arg(&review_graph),
            "--store",
            "--home",
            home_arg,
        ],
    )?;

    let dependent = fixture_pack_dir("dependent_fixture");
    let mut install_args = vec![
        "pack".to_owned(),
        "install".to_owned(),
        dir_arg(&dependent).to_owned(),
        "--home".to_owned(),
        home_arg.to_owned(),
        "--registry".to_owned(),
        UNROUTABLE_REGISTRY.to_owned(),
    ];
    for slot in &slots {
        install_args.push("--inference-slot".to_owned());
        install_args.push(slot.clone());
    }
    let install_args_ref: Vec<&str> = install_args.iter().map(String::as_str).collect();
    run_cli_json(tempdir.path(), &install_args_ref)?;

    let denial = run_cli_failure_stderr(
        tempdir.path(),
        &["pack", "remove", "fixture/review_graph", "--home", home_arg],
    )?;
    anyhow::ensure!(denial.contains("fixture/dependent_fixture"), "{denial}");

    let removed = run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "remove",
            "fixture/dependent_fixture",
            "--home",
            home_arg,
        ],
    )?;
    anyhow::ensure!(
        removed["removed"]["dependencies"][0]["pack"] == "fixture/review_graph",
        "{removed}"
    );

    let denial = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "graph",
            "enable",
            "fixture/review_graph",
            "--home",
            home_arg,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(
        denial.contains("package graph is not installed"),
        "{denial}"
    );

    // An explicit install of the (now-removed) dependency survives a later
    // removal of the dependent that names it again.
    let explicit_review_graph = [
        "pack",
        "install",
        dir_arg(&review_graph),
        "--home",
        home_arg,
        "--inference-slot",
        &format!("coordinator={profile}"),
        "--inference-slot",
        &format!("worker={profile}"),
        "--inference-slot",
        &format!("verifier={profile}"),
    ];
    run_cli_json(tempdir.path(), &explicit_review_graph)?;
    run_cli_json(tempdir.path(), &install_args_ref)?;
    run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "remove",
            "fixture/dependent_fixture",
            "--home",
            home_arg,
        ],
    )?;
    let enabled = run_cli_json(
        tempdir.path(),
        &[
            "graph",
            "enable",
            "fixture/review_graph",
            "--home",
            home_arg,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(
        enabled.get("enabled") == Some(&Value::Bool(true)),
        "an explicit install of the dependency must survive removing its dependent: {enabled}"
    );
    Ok(())
}

/// `graph run` compares the installed record, not a name it trusts blindly.
/// Corrupting the record's digest directly is out of scope for a CLI test,
/// so this proves the coordinate lookup itself is exact: a wrong namespace
/// names no installation record even though the plan resolves by bare name,
/// and removing the pack (which deletes the record) is refused distinctly
/// from a plain "not installed".
#[test]
fn graph_run_refuses_a_revision_that_is_not_the_installed_record() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating graph run tempdir")?;
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().context("path")?;
    let initialized = run_init_json(
        tempdir.path(),
        &["--agent-name", "graph-runner", "--home", home_arg],
    )?;
    let owner_did = agent_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    anyhow::ensure!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "server did not become ready: {readiness}"
    );
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let profile = format!("{owner_did}:default-profile");
    let pack = fixture_pack_dir("review_graph");
    run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "install",
            dir_arg(&pack),
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--agent-did",
            &owner_did,
            "--inference-slot",
            &format!("coordinator={profile}"),
            "--inference-slot",
            &format!("worker={profile}"),
            "--inference-slot",
            &format!("verifier={profile}"),
        ],
    )?;

    // The plan is found by name alone (`review_graph` matches, whatever
    // namespace the caller typed), but the run check reads the installed
    // record under the exact coordinate: a wrong namespace names no such
    // record, and must be refused there rather than silently running the
    // revision installed under a different coordinate.
    let denial = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "graph",
            "run",
            "review_graph",
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(
        denial.contains("gents/review_graph") && denial.contains("has no installation record"),
        "a bare-namespace run must be refused by its own coordinate, not the installed one: {denial}"
    );

    // Removing the pack deletes its graph definition along with the
    // record (proven by `a_graph_pack_removes_completely_and_reinstalls`),
    // so a run afterward is refused at the plan lookup, before the record
    // check this test exercises above ever runs.
    run_cli_json(
        tempdir.path(),
        &["pack", "remove", "fixture/review_graph", "--home", home_arg],
    )?;
    let denial = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "graph",
            "run",
            "fixture/review_graph",
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--agent-did",
            &owner_did,
        ],
    )?;
    anyhow::ensure!(denial.contains("graph is not installed"), "{denial}");
    Ok(())
}

/// A generic entry's declared `input_schema` is enforced through the CLI,
/// independent of the compiled-in catalog this binary no longer has.
#[test]
fn graph_run_validates_input_against_the_entry_schema() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating schema validation tempdir")?;
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().context("path")?;
    let initialized = run_init_json(
        tempdir.path(),
        &["--agent-name", "schema-runner", "--home", home_arg],
    )?;
    let owner_did = agent_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    anyhow::ensure!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "server did not become ready: {readiness}"
    );
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let profile = format!("{owner_did}:default-profile");

    // A private copy of `review_graph` with an injected `input_schema` on
    // its one entry: the fixture itself declares none, and this is the only
    // pack-shaped way to exercise `admit_operator_input` end to end through
    // the CLI without touching the checked-in fixture other tests share.
    let pack_dir = tempdir.path().join("review_graph_with_schema");
    copy_dir_all(&fixture_pack_dir("review_graph"), &pack_dir)?;
    let config_path = pack_dir.join("pack_config.json");
    let mut config: Value = serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;
    config["graph_intents"][0]["entries"][0]["input_schema"] = serde_json::json!({
        "type": "object",
        "properties": {
            "repository": {"type": "string", "pattern": "^[a-z0-9_./-]+$"}
        }
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;

    run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "install",
            dir_arg(&pack_dir),
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--agent-did",
            &owner_did,
            "--inference-slot",
            &format!("coordinator={profile}"),
            "--inference-slot",
            &format!("worker={profile}"),
            "--inference-slot",
            &format!("verifier={profile}"),
        ],
    )?;

    let denial = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "graph",
            "run",
            "fixture/review_graph",
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--agent-did",
            &owner_did,
            "--field",
            "repository=NOT VALID",
        ],
    )?;
    anyhow::ensure!(
        denial.contains("does not satisfy its schema"),
        "a value violating the pattern must be refused before any run starts: {denial}"
    );
    Ok(())
}

/// An entry's `git_diff` host step runs through the CLI and the pack's own
/// prepare plugin: the pack is built (compiling its plugin), installed from
/// a `.pack` file, and a run over a real two-commit repository persists the
/// plugin's evidence document and starts on the plugin's input, both carrying
/// the diff's head sha.
#[tokio::test]
async fn graph_run_prepares_git_diff_evidence_through_the_pack_plugin() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating prepare tempdir")?;
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().context("path")?;
    let initialized = run_init_json(
        tempdir.path(),
        &["--agent-name", "prepare-runner", "--home", home_arg],
    )?;
    let owner_did = agent_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    anyhow::ensure!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "server did not become ready: {readiness}"
    );
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let profile = format!("{owner_did}:default-profile");

    let pack_dir = tempdir.path().join("prepared_graph");
    copy_dir_all(&fixture_pack_dir("prepared_graph"), &pack_dir)?;
    let pack_file = tempdir.path().join("prepared_graph.pack");
    run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "build",
            dir_arg(&pack_dir),
            "--out",
            dir_arg(&pack_file),
        ],
    )?;
    run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "install",
            dir_arg(&pack_file),
            "--home",
            home_arg,
            "--grant-authority",
            "--agent-did",
            &owner_did,
            "--inference-slot",
            &format!("worker={profile}"),
        ],
    )?;

    let repo = tempdir.path().join("repo");
    std::fs::create_dir_all(&repo)?;
    let git = |args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["-c", "user.email=test@example.com", "-c", "user.name=Test"])
            .args(args)
            .output()
            .context("running git")?;
        anyhow::ensure!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    git(&["init", "--quiet"])?;
    std::fs::write(repo.join("a.txt"), "one\n")?;
    git(&["add", "-A"])?;
    git(&["commit", "--quiet", "-m", "base"])?;
    std::fs::write(repo.join("a.txt"), "two\n")?;
    git(&["add", "-A"])?;
    git(&["commit", "--quiet", "-m", "head"])?;
    let head = git(&["rev-parse", "HEAD"])?;

    let denial = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "graph",
            "run",
            "fixture/prepared_graph",
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--agent-did",
            &owner_did,
            "--field",
            &format!("repository={}", dir_arg(&repo)),
            "--field",
            "head=NOT VALID",
        ],
    )?;
    anyhow::ensure!(
        denial.contains("does not satisfy its schema"),
        "an invalid head must be refused before any host step runs: {denial}"
    );

    let receipt = run_cli_json(
        tempdir.path(),
        &[
            "graph",
            "run",
            "fixture/prepared_graph",
            "--output",
            "json",
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--agent-did",
            &owner_did,
            "--field",
            &format!("repository={}", dir_arg(&repo)),
        ],
    )?;
    anyhow::ensure!(receipt.get("run_id").is_some(), "{receipt}");

    let evidence = graphql_query(&graphql, "{ FixtureEvidence { head_ref note } }").await?;
    let row = first_graphql_row(&evidence, "FixtureEvidence")?;
    anyhow::ensure!(
        row["head_ref"] == head.as_str() && row["note"] == "prepared",
        "the plugin must have seen the repository's head {head}: {evidence}"
    );
    let jobs = graphql_query(&graphql, "{ FixtureJob { head_ref summary } }").await?;
    let job = first_graphql_row(&jobs, "FixtureJob")?;
    anyhow::ensure!(
        job["head_ref"] == head.as_str()
            && job["summary"] == format!("prepared at {head}").as_str(),
        "the run must start on the plugin's input: {jobs}"
    );
    Ok(())
}
