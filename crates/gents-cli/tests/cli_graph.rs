mod support;

type Result<T = (), E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;
use serde_json::Value;

use support::{
    allocate_port, copy_dir_all, first_graphql_row, fixture_pack_dir, graphql_query,
    node_did_from_init, run_cli_failure_stderr, run_cli_json, run_cli_text, run_init_json,
    spawn_server_with_ready_json,
};

const UNROUTABLE_REGISTRY: &str = "http://127.0.0.1:9";

fn required_str<'a>(value: &'a Value, path: &[&str]) -> Result<&'a str> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .ok_or_else(|| format!("missing JSON path {} in {value}", path.join(".")))?;
    }
    current
        .as_str()
        .ok_or_else(|| format!("JSON path {} is not a string", path.join(".")).into())
}

fn dir_arg(path: &std::path::Path) -> &str {
    path.to_str().expect("fixture path is not UTF-8")
}

#[test]
fn the_binary_embeds_no_packs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let listed = run_cli_json(temp.path(), &["pack", "list"])?;
    assert!(listed["packs"] == serde_json::json!([]), "{listed}");
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
    assert!(
        denial.contains("is not in the pack store of") && denial.contains("could not be reached"),
        "{denial}"
    );
    Ok(())
}

#[test]
fn document_pack_installs_without_seeding_and_is_idempotent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("node");
    let home_arg = home.to_str().expect("path");
    run_init_json(
        temp.path(),
        &["--node-name", "pack-installer", "--home", home_arg],
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
    assert!(first["apply"]["counts"]["Agent"] == 1, "{first}");
    let before_root = temp.path().join("before");
    run_cli_text(
        temp.path(),
        &[
            "config",
            "export",
            "--home",
            home_arg,
            "--root",
            before_root.to_str().expect("export path"),
        ],
    )?;
    let before = support::read_json_file(&before_root.join("pack_config.json"))?;
    let second = run_cli_json(temp.path(), &args)?;
    assert!(
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
            after_root.to_str().expect("export path"),
        ],
    )?;
    assert!(
        before == support::read_json_file(&after_root.join("pack_config.json"))?,
        "reinstall changed canonical configuration"
    );
    Ok(())
}

#[test]
fn clean_binary_install_is_idempotent_activates_and_is_owner_fenced() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating graph install tempdir");
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().expect("graph home path is not UTF-8");
    let initialized = run_init_json(
        tempdir.path(),
        &["--node-name", "graph-reviewer", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    assert!(
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
    assert!(
        first.get("install") == second.get("install"),
        "repeated install changed its durable receipt\nfirst: {first}\nsecond: {second}"
    );
    let revision = required_str(&first, &["install", "revision_digest"])?;
    assert!(
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
            "--node-did",
            wrong_actor,
        ],
    )?;
    assert!(
        denial.contains("package owner node is missing"),
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
            "--node-did",
            &owner_did,
        ],
    )?;
    assert!(disabled.get("enabled") == Some(&Value::Bool(false)));
    let enabled = run_cli_json(
        tempdir.path(),
        &[
            "graph",
            "enable",
            "fixture/review_graph",
            "--home",
            home_arg,
            "--node-did",
            &owner_did,
        ],
    )?;
    assert!(enabled.get("enabled") == Some(&Value::Bool(true)));
    Ok(())
}

/// A documents pack whose graph dependency installs into the same offline
/// home under the one store claim the pack install holds, while a second
/// holder of that home is still refused. The dependency is pre-stored with
/// `pack fetch --store` first, so the whole install runs with no registry.
#[test]
fn offline_pack_install_with_a_graph_dependency_holds_one_store_claim() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating pack install tempdir");
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().expect("pack home path is not UTF-8");
    let initialized = run_init_json(
        tempdir.path(),
        &["--node-name", "port-installer", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;

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
    let dependent = dependent_on_review_graph(tempdir.path())?;
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
    assert!(
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
            "--node-did",
            &owner_did,
        ],
    )?;
    assert!(
        enabled.get("enabled") == Some(&Value::Bool(true)),
        "the graph dependency was not installed: {enabled}"
    );
    let record = gents::pack::read_home_install(&home, "fixture/dependent_fixture")?
        .expect("the dependent pack wrote no home record");
    assert!(
        record.dependency_assets.len() == 1
            && record.dependency_assets[0].0 == "fixture/review_graph"
            && home
                .join(&record.dependency_assets[0].1)
                .join("agents/review_recon/system_prompt.md")
                .is_file(),
        "the dependent's record does not name its dependency's assets: {record:?}"
    );

    let _held = gents::home::lock_store(&home, &gents::home::default_data_dir(&home))?;
    let denial = run_cli_failure_stderr(tempdir.path(), &install_args_ref)?;
    assert!(
        denial.contains("another Gents runtime")
            && denial.contains(&format!("process {}", std::process::id())),
        "a second holder of the home was not refused: {denial}"
    );
    Ok(())
}

/// A private copy of `dependent_fixture` depending on `review_graph`, so the
/// graph dependency path stays covered now that the checked-in fixture
/// depends on a documents pack.
fn dependent_on_review_graph(parent: &std::path::Path) -> Result<std::path::PathBuf> {
    let dependent = parent.join("dependent_on_review_graph");
    copy_dir_all(&fixture_pack_dir("dependent_fixture"), &dependent)?;
    let manifest_path = dependent.join("manifest.json");
    let mut manifest: Value = serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
    manifest["dependencies"] = serde_json::json!(["fixture/review_graph"]);
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(dependent)
}

#[test]
fn an_assets_pack_removes_completely_from_an_uninitialized_home() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("assets-home");
    let home_arg = home.to_str().expect("path");
    let pack = fixture_pack_dir("assets_fixture");

    let install = run_cli_json(
        temp.path(),
        &["pack", "install", dir_arg(&pack), "--home", home_arg],
    )?;
    let installed_assets = std::path::Path::new(required_str(&install, &["installed_assets"])?);
    assert!(installed_assets.is_dir(), "{install}");
    assert!(
        installed_assets.join("data/nested.json").is_file(),
        "{install}"
    );
    assert!(
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
    assert!(
        removed["removed"]["assets"]
            .as_array()
            .expect("assets")
            .len()
            == 1,
        "{removed}"
    );
    assert!(!installed_assets.exists(), "the cache version was removed");
    assert!(
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
    assert!(again.contains("is not installed"), "{again}");
    Ok(())
}

#[test]
fn a_graph_pack_removes_completely_and_reinstalls() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating graph remove tempdir");
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().expect("path");
    let initialized = run_init_json(
        tempdir.path(),
        &["--node-name", "graph-remover", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;
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
    let retained = removed["removed"]["retained"].as_array().expect("retained");
    assert!(
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
            "--node-did",
            &owner_did,
        ],
    )?;
    assert!(
        denial.contains("package graph is not installed"),
        "{denial}"
    );

    run_cli_json(tempdir.path(), &install_args)?;
    Ok(())
}

#[test]
fn a_graph_dependency_is_released_with_its_dependent() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating dependency remove tempdir");
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().expect("path");
    let initialized = run_init_json(
        tempdir.path(),
        &["--node-name", "dep-remover", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;
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

    let dependent = dependent_on_review_graph(tempdir.path())?;
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
    assert!(denial.contains("fixture/dependent_fixture"), "{denial}");

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
    assert!(
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
            "--node-did",
            &owner_did,
        ],
    )?;
    assert!(
        denial.contains("package graph is not installed"),
        "{denial}"
    );

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
            "--node-did",
            &owner_did,
        ],
    )?;
    assert!(
        enabled.get("enabled") == Some(&Value::Bool(true)),
        "an explicit install of the dependency must survive removing its dependent: {enabled}"
    );
    Ok(())
}

#[test]
fn graph_run_refuses_a_revision_that_is_not_the_installed_record() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating graph run tempdir");
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().expect("path");
    let initialized = run_init_json(
        tempdir.path(),
        &["--node-name", "graph-runner", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    assert!(
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
            "--node-did",
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
            "review_graph",
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--node-did",
            &owner_did,
        ],
    )?;
    assert!(
        denial.contains("gents/review_graph") && denial.contains("has no installation record"),
        "a bare-namespace run must be refused by its own coordinate, not the installed one: {denial}"
    );

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
            "--node-did",
            &owner_did,
        ],
    )?;
    assert!(denial.contains("graph is not installed"), "{denial}");
    Ok(())
}

#[test]
fn graph_run_validates_input_against_the_entry_schema() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating schema validation tempdir");
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().expect("path");
    let initialized = run_init_json(
        tempdir.path(),
        &["--node-name", "schema-runner", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    assert!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "server did not become ready: {readiness}"
    );
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let profile = format!("{owner_did}:default-profile");

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
            "--node-did",
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
            "--node-did",
            &owner_did,
            "--field",
            "repository=NOT VALID",
        ],
    )?;
    assert!(
        denial.contains("does not satisfy its schema"),
        "a value violating the pattern must be refused before any run starts: {denial}"
    );
    Ok(())
}

#[test]
fn graph_run_by_graph_id_is_pinned_to_the_active_digest() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating graph-id run tempdir");
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().expect("path");
    let initialized = run_init_json(
        tempdir.path(),
        &["--node-name", "graph-id-runner", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    assert!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "server did not become ready: {readiness}"
    );
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let profile = format!("{owner_did}:default-profile");
    let pack = fixture_pack_dir("review_graph");
    let installed = run_cli_json(
        tempdir.path(),
        &[
            "pack",
            "install",
            dir_arg(&pack),
            "--home",
            home_arg,
            "--graphql",
            &graphql,
            "--node-did",
            &owner_did,
            "--output",
            "json",
            "--inference-slot",
            &format!("coordinator={profile}"),
            "--inference-slot",
            &format!("worker={profile}"),
            "--inference-slot",
            &format!("verifier={profile}"),
        ],
    )?;
    let graph_id = required_str(&installed, &["install", "graph_id"])?;
    let digest = required_str(&installed, &["install", "revision_digest"])?;
    let scope = [
        "--home",
        home_arg,
        "--graphql",
        graphql.as_str(),
        "--node-did",
        owner_did.as_str(),
    ];

    let stale = format!("sha256:{}", "0".repeat(64));
    let mut args = vec![
        "graph",
        "run",
        "--graph-id",
        graph_id,
        "--digest",
        stale.as_str(),
    ];
    args.extend(scope);
    let denial = run_cli_failure_stderr(tempdir.path(), &args)?;
    assert!(
        denial.contains(&format!("is active at revision {digest}")),
        "a digest that is not the active revision must be refused before any run starts: {denial}"
    );

    let mut args = vec![
        "graph",
        "run",
        "--graph-id",
        graph_id,
        "--digest",
        digest,
        "--output",
        "ndjson",
    ];
    args.extend(scope);
    let printed = run_cli_text(tempdir.path(), &args)?;
    let lines: Vec<&str> = printed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        lines.len() == 1,
        "ndjson prints exactly one receipt line: {printed}"
    );
    let receipt: Value = serde_json::from_str(lines[0]).expect("the receipt line is JSON");
    assert!(
        required_str(&receipt, &["graph_id"])? == graph_id
            && required_str(&receipt, &["revision_digest"])? == digest,
        "{receipt}"
    );
    let run_id = required_str(&receipt, &["run_id"])?;
    let mut args = vec!["graph", "cancel", run_id, "--reason", "graph-id acceptance"];
    args.extend(scope);
    run_cli_json(tempdir.path(), &args)?;
    Ok(())
}

#[tokio::test]
async fn graph_run_prepares_git_diff_evidence_through_the_pack_plugin() -> Result<()> {
    let tempdir = tempfile::tempdir().expect("creating prepare tempdir");
    let home = tempdir.path().join("agent-home");
    let home_arg = home.to_str().expect("path");
    let initialized = run_init_json(
        tempdir.path(),
        &["--node-name", "prepare-runner", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;
    let port = allocate_port()?;
    let (_server, readiness) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[])?;
    assert!(
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
            "--node-did",
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
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("running git");
        assert!(
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
            "--node-did",
            &owner_did,
            "--field",
            &format!("repository={}", dir_arg(&repo)),
            "--field",
            "head=NOT VALID",
        ],
    )?;
    assert!(
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
            "--node-did",
            &owner_did,
            "--field",
            &format!("repository={}", dir_arg(&repo)),
        ],
    )?;
    assert!(receipt.get("run_id").is_some(), "{receipt}");

    let evidence = graphql_query(&graphql, "{ FixtureEvidence { head_ref note } }").await?;
    let row = first_graphql_row(&evidence, "FixtureEvidence")?;
    assert!(
        row["head_ref"] == head.as_str() && row["note"] == "prepared",
        "the plugin must have seen the repository's head {head}: {evidence}"
    );
    let jobs = graphql_query(&graphql, "{ FixtureJob { head_ref summary } }").await?;
    let job = first_graphql_row(&jobs, "FixtureJob")?;
    assert!(
        job["head_ref"] == head.as_str()
            && job["summary"] == format!("prepared at {head}").as_str(),
        "the run must start on the plugin's input: {jobs}"
    );
    Ok(())
}
