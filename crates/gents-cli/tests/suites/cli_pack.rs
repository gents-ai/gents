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
        Self::with_dependee(node_name, &fixture_pack_dir("dependee_fixture"))
    }

    fn with_dependee(node_name: &str, dependee: &std::path::Path) -> Result<Self> {
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
                dir_arg(dependee),
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

    let _held = gents::store::local::home::lock_store(
        &setup.home,
        &gents::store::local::home::default_data_dir(&setup.home),
    )?;
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

/// A documents pack whose Tools name a plugin that neither it nor the node
/// provides is refused by `gents pack install` before any document is written.
#[test]
fn offline_pack_install_naming_an_unresolved_plugin_is_refused_before_writing() -> Result<()> {
    let scratch = tempfile::tempdir().context("creating pack tempdir")?;
    let pack = scratch.path().join("dependee_fixture");
    copy_dir_all(&fixture_pack_dir("dependee_fixture"), &pack)?;
    let config_path = pack.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    config["tools"][0]["integrations"] =
        serde_json::json!({"plugins": [{"plugin": "acme/missing"}]});
    std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;

    let home = scratch.path().join("agent-home");
    let home_arg = dir_arg(&home);
    let initialized = run_init_json(
        scratch.path(),
        &["--node-name", "unresolved-plugin", "--home", home_arg],
    )?;
    let owner_did = node_did_from_init(&initialized)?;
    let refusal = run_cli_failure_stderr(
        scratch.path(),
        &[
            "pack",
            "install",
            dir_arg(&pack),
            "--home",
            home_arg,
            "--registry",
            UNROUTABLE_REGISTRY,
            "--inference-slot",
            &format!("worker={owner_did}:default-profile"),
        ],
    )?;
    anyhow::ensure!(
        refusal.contains("acme/missing") && refusal.contains("gents pack install"),
        "the unresolved plugin was not named: {refusal}"
    );

    let export = scratch.path().join("export");
    run_cli_text(
        scratch.path(),
        &[
            "config",
            "export",
            "--root",
            dir_arg(&export),
            "--home",
            home_arg,
        ],
    )?;
    let exported = read_json_file(&export.join("pack_config.json"))?;
    anyhow::ensure!(
        !exported.to_string().contains("fixture-dependee-tools"),
        "a Tools document was written by the refused install: {exported}"
    );
    Ok(())
}

/// A documents dependency that ships a plugin installs that plugin under the
/// dependency's own coordinate, and removing the dependent releases it.
#[test]
fn a_documents_dependency_plugin_is_released_with_its_dependent() -> Result<()> {
    let scratch = tempfile::tempdir().context("creating dependee tempdir")?;
    let dependee = scratch.path().join("dependee_fixture");
    copy_dir_all(&fixture_pack_dir("dependee_fixture"), &dependee)?;
    run_cli_text(
        scratch.path(),
        &[
            "pack",
            "add",
            "plugin",
            "echo_tool",
            "--dir",
            dir_arg(&dependee),
        ],
    )?;
    run_cli_text(scratch.path(), &["pack", "build", dir_arg(&dependee)])?;

    let setup = DependentHome::with_dependee("dep-plugin", &dependee)?;
    run_cli_json(setup.tempdir.path(), &setup.install_args())?;
    let record = gents::plugin::store::read_record(&setup.home, "fixture", "echo_tool")?;
    anyhow::ensure!(
        record.owner_pack_coordinate.as_deref() == Some("fixture/dependee_fixture"),
        "the dependency's plugin is not owned by the dependency: {record:?}"
    );

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
    anyhow::ensure!(
        gents::plugin::store::read_record(&setup.home, "fixture", "echo_tool").is_err(),
        "the dependency's plugin outlived its dependent"
    );
    Ok(())
}

const FOREIGN_NODE: &str = "did:key:zWireFixtureForeign";

/// An initialized offline home with the job plugin's pack and a content pack
/// (one AgentTarget, no plugin) installed, and the agent `init` created.
struct WireFixture {
    tempdir: tempfile::TempDir,
    home: std::path::PathBuf,
    agent: String,
}

fn wire_target(id: &str) -> serde_json::Value {
    serde_json::json!({
        "target_id": id,
        "target_node_did": FOREIGN_NODE,
        "agent_id": id,
        "name": id,
    })
}

fn edit_pack_config(
    dir: &std::path::Path,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Result<()> {
    let path = dir.join("pack_config.json");
    let mut config: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    edit(&mut config);
    write_json_file(&path, &config)
}

/// `reviewed_job` false ships the job pack's Tools entry without a `job` key.
fn wire_fixture(reviewed_job: bool) -> Result<WireFixture> {
    wire_fixture_with(reviewed_job, &[])
}

/// `wire_fixture` with `init_extra` appended to the `gents init` arguments.
fn wire_fixture_with(reviewed_job: bool, init_extra: &[&str]) -> Result<WireFixture> {
    let tempdir = tempfile::tempdir().context("creating wire tempdir")?;
    let scratch = tempdir.path();
    let home = scratch.join("agent-home");
    let home_arg = dir_arg(&home);
    let mut init_args = vec!["--node-name", "pack-wire", "--home", home_arg];
    init_args.extend_from_slice(init_extra);
    let initialized = run_init_json(scratch, &init_args)?;
    let owner_did = node_did_from_init(&initialized)?;
    let agent = initialized["default_agent_id"]
        .as_str()
        .context("init output missing default_agent_id")?
        .to_owned();

    let job_pack = scratch.join("job_plugin_fixture");
    copy_dir_all(&fixture_pack_dir("job_plugin_fixture"), &job_pack)?;
    let mut plugin_entry = serde_json::json!({"plugin": "fixture/job_fixture"});
    if reviewed_job {
        plugin_entry["job"] =
            serde_json::json!({"agent_targets": ["wf-fixture"], "policy": {"dynamic": "off"}});
    }
    edit_pack_config(&job_pack, |config| {
        *config = serde_json::json!({
            "node": {},
            "agent_targets": [wire_target("wf-fixture")],
            "tools": [{
                "tools_id": "job-fixture-caller",
                "display_name": "Job fixture caller",
                "integrations": {"plugins": [plugin_entry]},
            }],
        });
    })?;

    let content_pack = scratch.join("dependee_fixture");
    copy_dir_all(&fixture_pack_dir("dependee_fixture"), &content_pack)?;
    edit_pack_config(&content_pack, |config| {
        config["agent_targets"] = serde_json::json!([wire_target("wf-content")]);
    })?;

    run_cli_text(scratch, &["pack", "build", dir_arg(&job_pack)])?;
    for (pack, slots) in [
        (&job_pack, Vec::new()),
        (
            &content_pack,
            vec![
                "--inference-slot".to_owned(),
                format!("worker={owner_did}:default-profile"),
            ],
        ),
    ] {
        let mut args = vec![
            "pack",
            "install",
            dir_arg(pack),
            "--home",
            home_arg,
            "--registry",
            UNROUTABLE_REGISTRY,
        ];
        args.extend(slots.iter().map(String::as_str));
        run_cli_json(scratch, &args)?;
    }
    Ok(WireFixture {
        tempdir,
        home,
        agent,
    })
}

impl WireFixture {
    fn wire(&self, extra: &[&str]) -> Result<serde_json::Value> {
        let mut args = vec![
            "pack",
            "wire",
            "fixture/job_fixture",
            "--agent",
            &self.agent,
            "--home",
            dir_arg(&self.home),
        ];
        args.extend_from_slice(extra);
        run_cli_json(self.tempdir.path(), &args)
    }

    fn tools(&self, tools_id: &str) -> Result<serde_json::Value> {
        run_cli_json(
            self.tempdir.path(),
            &[
                "config",
                "tools",
                "show",
                tools_id,
                "--home",
                dir_arg(&self.home),
            ],
        )
    }

    fn agent_tools(&self) -> Result<serde_json::Value> {
        self.tools(&format!("{}-tools", self.agent))
    }
}

fn wired_plugin(tools: &serde_json::Value) -> Option<&serde_json::Value> {
    tools["integrations"]["plugins"]
        .as_array()?
        .iter()
        .find(|entry| entry["plugin"] == "fixture/job_fixture")
}

fn plugin_coordinates(tools: &serde_json::Value) -> std::collections::BTreeSet<String> {
    tools["integrations"]["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["plugin"].as_str().map(str::to_owned))
        .collect()
}

fn target_ids(tools: &serde_json::Value) -> std::collections::BTreeSet<String> {
    tools["agents"]["target_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect()
}

#[test]
fn pack_wire_writes_tools_and_context_in_one_transaction() -> Result<()> {
    let fixture = wire_fixture(true)?;
    fixture.wire(&[])?;
    let plugin = gents::plugin::store::read_record(&fixture.home, "fixture", "job_fixture")?;
    let tools = fixture.agent_tools()?;
    let entry = wired_plugin(&tools).context("the agent's Tools does not list the plugin")?;
    anyhow::ensure!(
        entry["digest"] == plugin.digest.as_str(),
        "the plugin is not pinned to the installed digest: {entry}"
    );
    Ok(())
}

#[test]
fn pack_wire_uses_the_packs_reviewed_block() -> Result<()> {
    let fixture = wire_fixture(true)?;
    let reviewed = wired_plugin(&fixture.tools("job-fixture-caller")?)
        .context("the job pack's Tools does not list the plugin")?["job"]
        .clone();
    let printed = fixture.wire(&[])?;
    let tools = fixture.agent_tools()?;
    let written = &wired_plugin(&tools).context("plugin not wired")?["job"];
    anyhow::ensure!(
        *written == reviewed && printed["report"]["job"] == reviewed,
        "the written block {written} or the report {} differs from the reviewed {reviewed}",
        printed["report"]["job"]
    );
    Ok(())
}

#[test]
fn pack_wire_targets_from_adds_targets_and_trust() -> Result<()> {
    let fixture = wire_fixture(true)?;
    fixture.wire(&["--targets-from", "fixture/dependee_fixture"])?;
    let tools = fixture.agent_tools()?;
    let job = &wired_plugin(&tools).context("plugin not wired")?["job"];
    anyhow::ensure!(
        job["agent_targets"]
            .as_array()
            .is_some_and(|targets| targets.iter().any(|id| id == "wf-content"))
            && target_ids(&tools).contains("wf-content")
            && job["policy"]["trusted_packs"]
                .as_array()
                .is_some_and(|packs| packs.iter().any(|pack| pack == "fixture/dependee_fixture")),
        "the content pack's target and trust were not wired: {tools}"
    );
    Ok(())
}

#[test]
fn pack_wire_without_targets_writes_no_job_block() -> Result<()> {
    let fixture = wire_fixture(false)?;
    let printed = fixture.wire(&[])?;
    let tools = fixture.agent_tools()?;
    anyhow::ensure!(
        wired_plugin(&tools).is_some_and(|entry| entry.get("job").is_none()),
        "a job block was written without targets: {tools}"
    );
    anyhow::ensure!(
        printed["report"]["next_call"]
            .as_str()
            .is_some_and(|call| call.contains("--targets-from <pack>")),
        "the report does not name --targets-from: {printed}"
    );
    Ok(())
}

/// `checks.plugin_jobs` is reported, and its failure alone degrades `status`.
#[test]
fn diagnose_status_degrades_on_a_plugin_jobs_failure() -> Result<()> {
    // A reachable backend keeps every other check green, so only plugin_jobs can degrade status.
    let model_name = "mock-plugin-jobs-model";
    let endpoint = MockModelEndpoint::start(model_name)?;
    let fixture = wire_fixture_with(
        false,
        &[
            "--model-name",
            model_name,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let diagnose = || {
        run_cli_json(
            fixture.tempdir.path(),
            &["diagnose", "--home", dir_arg(&fixture.home)],
        )
    };
    let before = diagnose()?;
    anyhow::ensure!(
        before["checks"]["plugin_jobs"]["ok"] == true && before["status"] == "ok",
        "an unwired home is not healthy: {before}"
    );
    fixture.wire(&[])?;
    let after = diagnose()?;
    anyhow::ensure!(
        after["checks"]["plugin_jobs"]["ok"] == false && after["status"] == "degraded",
        "a plugin_jobs failure did not degrade status: {after}"
    );
    anyhow::ensure!(
        after["checks"]["plugin_jobs"]["agents"][0]["checks"][0]["check"] == "job_block_missing",
        "unexpected plugin_jobs row: {after}"
    );
    Ok(())
}

#[test]
fn pack_wire_preview_writes_nothing() -> Result<()> {
    let fixture = wire_fixture(true)?;
    let before = fixture.agent_tools()?;
    let previewed = fixture.wire(&["--preview"])?;
    anyhow::ensure!(
        fixture.agent_tools()? == before,
        "a preview changed the agent's Tools"
    );
    let wired = fixture.wire(&[])?;
    anyhow::ensure!(
        previewed["preview"] == true && wired["preview"] == false,
        "the preview flag is not reported: {previewed} {wired}"
    );
    anyhow::ensure!(
        previewed["report"] == wired["report"],
        "the preview report differs from the written one: {previewed} {wired}"
    );
    Ok(())
}

#[test]
fn pack_wire_remove_reverses_only_unreferenced() -> Result<()> {
    let fixture = wire_fixture(true)?;
    let before = fixture.agent_tools()?;
    fixture.wire(&["--targets-from", "fixture/dependee_fixture"])?;
    let wired = fixture.agent_tools()?;
    anyhow::ensure!(
        wired_plugin(&wired).is_some(),
        "the wire did not list the plugin: {wired}"
    );
    fixture.wire(&["--remove"])?;
    let after = fixture.agent_tools()?;
    anyhow::ensure!(
        wired_plugin(&after).is_none()
            && plugin_coordinates(&after) == plugin_coordinates(&before)
            && target_ids(&after) == target_ids(&before),
        "removing did not restore the plugin list and target ids: {after}"
    );
    Ok(())
}

#[test]
fn pack_wire_refuses_an_agent_without_a_tools_document() -> Result<()> {
    let fixture = wire_fixture(true)?;
    let root = fixture.tempdir.path().join("config");
    let (root_arg, home_arg) = (dir_arg(&root), dir_arg(&fixture.home));
    run_cli_text(
        fixture.tempdir.path(),
        &["config", "export", "--root", root_arg, "--home", home_arg],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    let context_id = format!("{}-context", fixture.agent);
    let context = config["contexts"]
        .as_array_mut()
        .and_then(|contexts| {
            contexts
                .iter_mut()
                .find(|c| c["context_id"] == context_id.as_str())
        })
        .context("the agent's Context is not in the export")?;
    context
        .as_object_mut()
        .context("Context is not an object")?
        .remove("tools_id");
    write_json_file(&path, &config)?;
    run_cli_json(
        fixture.tempdir.path(),
        &["config", "apply", "--root", root_arg, "--home", home_arg],
    )?;

    let refusal = run_cli_failure_stderr(
        fixture.tempdir.path(),
        &[
            "pack",
            "wire",
            "fixture/job_fixture",
            "--agent",
            &fixture.agent,
            "--home",
            home_arg,
        ],
    )?;
    anyhow::ensure!(
        refusal.contains(&format!("gents tools explain --agent {}", fixture.agent)),
        "the refusal does not name the next call: {refusal}"
    );
    Ok(())
}

const JOB_PACK: &str = "fixture/job_plugin_fixture";

/// An initialized offline home with the job pack built in the scratch
/// directory and nothing installed.
fn uninstalled_job_pack() -> Result<WireFixture> {
    let tempdir = tempfile::tempdir().context("creating install tempdir")?;
    let scratch = tempdir.path();
    let home = scratch.join("agent-home");
    let initialized = run_init_json(
        scratch,
        &["--node-name", "pack-digest", "--home", dir_arg(&home)],
    )?;
    let agent = initialized["default_agent_id"]
        .as_str()
        .context("init output missing default_agent_id")?
        .to_owned();
    let job_pack = scratch.join("job_plugin_fixture");
    copy_dir_all(&fixture_pack_dir("job_plugin_fixture"), &job_pack)?;
    run_cli_text(scratch, &["pack", "build", dir_arg(&job_pack)])?;
    Ok(WireFixture {
        tempdir,
        home,
        agent,
    })
}

impl WireFixture {
    fn job_pack(&self) -> std::path::PathBuf {
        self.tempdir.path().join("job_plugin_fixture")
    }

    fn job_pack_digest(&self) -> Result<String> {
        let shown = run_cli_json(
            self.tempdir.path(),
            &["pack", "show", dir_arg(&self.job_pack())],
        )?;
        Ok(shown["digest"]
            .as_str()
            .context("pack show has no digest")?
            .to_owned())
    }

    fn install_job_pack_args(&self, extra: &[&str]) -> Vec<String> {
        let mut args = vec![
            "pack".to_owned(),
            "install".to_owned(),
            dir_arg(&self.job_pack()).to_owned(),
            "--home".to_owned(),
            dir_arg(&self.home).to_owned(),
            "--registry".to_owned(),
            UNROUTABLE_REGISTRY.to_owned(),
        ];
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        args
    }

    fn install_job_pack(&self, extra: &[&str]) -> Result<serde_json::Value> {
        let args = self.install_job_pack_args(extra);
        run_cli_json(
            self.tempdir.path(),
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }

    fn install_job_pack_failure(&self, extra: &[&str]) -> Result<String> {
        let args = self.install_job_pack_args(extra);
        run_cli_failure_stderr(
            self.tempdir.path(),
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }
}

#[test]
fn pack_install_expect_digest_match_installs() -> Result<()> {
    let fixture = uninstalled_job_pack()?;
    let digest = fixture.job_pack_digest()?;
    fixture.install_job_pack(&["--expect-digest", &digest])?;
    anyhow::ensure!(
        gents::pack::read_home_install(&fixture.home, JOB_PACK)?.is_some(),
        "a matching --expect-digest did not install the pack"
    );
    Ok(())
}

#[test]
fn pack_install_expect_digest_mismatch_writes_nothing() -> Result<()> {
    let fixture = uninstalled_job_pack()?;
    let digest = fixture.job_pack_digest()?;
    let wrong = format!("sha256:{}", "0".repeat(64));
    let stderr = fixture.install_job_pack_failure(&["--expect-digest", &wrong])?;
    for needle in [wrong.as_str(), digest.as_str(), "gents pack show"] {
        anyhow::ensure!(stderr.contains(needle), "{needle} missing from: {stderr}");
    }
    anyhow::ensure!(
        gents::pack::read_home_install(&fixture.home, JOB_PACK)?.is_none()
            && gents::plugin::store::list_records(&fixture.home)?.is_empty(),
        "a refused digest left an install behind"
    );
    Ok(())
}

#[test]
fn pack_install_wire_wires_the_agent() -> Result<()> {
    let fixture = wire_fixture(true)?;
    let reviewed = wired_plugin(&fixture.tools("job-fixture-caller")?)
        .context("the job pack's Tools does not list the plugin")?["job"]
        .clone();
    let digest = fixture.job_pack_digest()?;
    let printed =
        fixture.install_job_pack(&["--expect-digest", &digest, "--wire", &fixture.agent])?;
    let wired = &printed["wired"][0];
    anyhow::ensure!(
        wired["plugin"] == "fixture/job_fixture"
            && wired["agent"] == fixture.agent.as_str()
            && wired["report"]["job"] == reviewed,
        "install did not print the wired report for the reviewed block: {printed}"
    );
    anyhow::ensure!(
        wired_plugin(&fixture.agent_tools()?).is_some(),
        "the agent's Tools does not list the plugin"
    );
    Ok(())
}

#[test]
fn pack_install_wire_refuses_before_any_write_without_grant() -> Result<()> {
    let fixture = wire_fixture(true)?;
    let manifest_path = fixture.job_pack().join("manifest.json");
    let mut manifest = read_json_file(&manifest_path)?;
    manifest["plugins"][0]["manifold"] = serde_json::json!({
        "fs": "None",
        "net": {"OutboundHttp": ["api.example.com"]},
        "env": "None",
        "crypto": false,
        "child_process": false,
    });
    write_json_file(&manifest_path, &manifest)?;
    let records = serde_json::to_value(gents::plugin::store::list_records(&fixture.home)?)?;
    let tools = fixture.agent_tools()?;

    let stderr = fixture.install_job_pack_failure(&["--wire", &fixture.agent])?;
    anyhow::ensure!(
        stderr.contains("--grant-authority"),
        "the refusal does not name --grant-authority: {stderr}"
    );
    anyhow::ensure!(
        serde_json::to_value(gents::plugin::store::list_records(&fixture.home)?)? == records
            && fixture.agent_tools()? == tools,
        "a refused install changed the plugin store or the agent's Tools"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_sources_lists_job_plugins_and_target_packs() -> Result<()> {
    let fixture = wire_fixture(true)?;
    let sources = gents_server::packs::wire_sources(fixture.home.clone()).await?;
    let plugins = sources["plugins"]
        .as_array()
        .context("wire_sources printed no plugins")?;
    anyhow::ensure!(
        plugins.contains(&serde_json::json!({
            "plugin": "fixture/job_fixture",
            "pack": "fixture/job_plugin_fixture",
            "job": true,
        })),
        "the job plugin is not listed with its pack and job flag: {sources}"
    );
    let target_packs = sources["target_packs"]
        .as_array()
        .context("wire_sources printed no target_packs")?;
    for expected in [
        serde_json::json!({"pack": "fixture/dependee_fixture", "targets": ["wf-content"]}),
        serde_json::json!({"pack": "fixture/job_plugin_fixture", "targets": ["wf-fixture"]}),
    ] {
        anyhow::ensure!(
            target_packs.contains(&expected),
            "target_packs does not hold {expected}: {sources}"
        );
    }
    Ok(())
}
