use crate::support::{self, *};

use anyhow::{Context, Result};

/// A documents pack that ships a plugin: install stores the plugin and
/// records it. No Tools selects the plugin, so removing the pack retires it:
/// the plugin is no longer installed or callable. Every pack here is
/// scaffolded on the fly, so this needs no fixture at all.
#[test]
fn a_document_pack_with_a_plugin_installs_and_its_removal_retires_the_plugin() -> Result<()> {
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
        &["config", "pack", "new", "demo_tools"][..],
        &[
            "config",
            "pack",
            "add",
            "plugin",
            "echo_tool",
            "--dir",
            "demo_tools",
        ],
        &[
            "config",
            "pack",
            "build",
            "demo_tools",
            "--out",
            "demo_tools.pack",
        ],
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
            "config",
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
            "config",
            "pack",
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

    run_cli_json(
        temp.path(),
        &["config", "pack", "remove", "demo_tools", "--home", home_arg],
    )?;
    anyhow::ensure!(
        gents_pack::plugin::store::read_record(&home, "gents", "echo_tool").is_err(),
        "the retired plugin still has a store record"
    );
    let uncallable = run_cli_failure_stderr(
        temp.path(),
        &[
            "config",
            "pack",
            "plugin",
            "run",
            "gents/echo_tool",
            "--home",
            home_arg,
            "--input",
            r#"{"b":2}"#,
        ],
    )?;
    anyhow::ensure!(uncallable.contains("not installed"), "{uncallable}");
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
        node_did_from_init(&initialized)?;
        run_cli_json(
            tempdir.path(),
            &[
                "config",
                "pack",
                "fetch",
                dir_arg(dependee),
                "--store",
                "--home",
                home_arg,
            ],
        )?;
        let install_args = [
            "config",
            "pack",
            "install",
            dir_arg(&fixture_pack_dir("dependent_fixture")),
            "--home",
            home_arg,
            "--registry",
            UNROUTABLE_REGISTRY,
            "--inference-slot",
            &format!("worker={}", inference_profile_id_from_init(&initialized)?),
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
            "config",
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

    let _held = gents_store::local::home::lock_store(
        &setup.home,
        &gents_store::local::home::default_data_dir(&setup.home),
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
            "config",
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
            "config",
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
            "config",
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
            "config",
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
            "config",
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
            "config",
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
/// provides is refused by `gents config pack install` before any document is written.
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
    node_did_from_init(&initialized)?;
    let refusal = run_cli_failure_stderr(
        scratch.path(),
        &[
            "config",
            "pack",
            "install",
            dir_arg(&pack),
            "--home",
            home_arg,
            "--registry",
            UNROUTABLE_REGISTRY,
            "--inference-slot",
            &format!("worker={}", inference_profile_id_from_init(&initialized)?),
        ],
    )?;
    anyhow::ensure!(
        refusal.contains("acme/missing") && refusal.contains("gents config pack install"),
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
/// dependency's own coordinate. Removing the dependent releases the
/// dependency, and with it retires the plugin no Tools selects.
#[test]
fn a_documents_dependency_plugin_is_retired_with_its_dependent() -> Result<()> {
    let scratch = tempfile::tempdir().context("creating dependee tempdir")?;
    let dependee = scratch.path().join("dependee_fixture");
    copy_dir_all(&fixture_pack_dir("dependee_fixture"), &dependee)?;
    run_cli_text(
        scratch.path(),
        &[
            "config",
            "pack",
            "add",
            "plugin",
            "echo_tool",
            "--dir",
            dir_arg(&dependee),
        ],
    )?;
    run_cli_text(
        scratch.path(),
        &["config", "pack", "build", dir_arg(&dependee)],
    )?;

    let setup = DependentHome::with_dependee("dep-plugin", &dependee)?;
    run_cli_json(setup.tempdir.path(), &setup.install_args())?;
    let record = gents_pack::plugin::store::read_record(&setup.home, "fixture", "echo_tool")?;
    anyhow::ensure!(
        record.owner_pack_coordinate.as_deref() == Some("fixture/dependee_fixture"),
        "the dependency's plugin is not owned by the dependency: {record:?}"
    );

    run_cli_json(
        setup.tempdir.path(),
        &[
            "config",
            "pack",
            "remove",
            "fixture/dependent_fixture",
            "--home",
            setup.home_arg(),
        ],
    )?;
    anyhow::ensure!(
        gents_pack::plugin::store::read_record(&setup.home, "fixture", "echo_tool").is_err(),
        "the dependency's plugin outlived the removal that released it"
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
        "agent_did": id,
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
    node_did_from_init(&initialized)?;
    let agent = initialized["default_agent_did"]
        .as_str()
        .context("init output missing default_agent_did")?
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

    run_cli_text(scratch, &["config", "pack", "build", dir_arg(&job_pack)])?;
    for (pack, slots) in [
        (&job_pack, Vec::new()),
        (
            &content_pack,
            vec![
                "--inference-slot".to_owned(),
                format!("worker={}", inference_profile_id_from_init(&initialized)?),
            ],
        ),
    ] {
        let mut args = vec![
            "config",
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
            "config",
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
    let plugin = gents_pack::plugin::store::read_record(&fixture.home, "fixture", "job_fixture")?;
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
            &["runtime", "diagnose", "--home", dir_arg(&fixture.home)],
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
    let wired = after["checks"]["plugin_jobs"]["agents"]
        .as_array()
        .context("plugin_jobs lists no agents")?
        .iter()
        .find(|row| row["agent_did"] == fixture.agent.as_str())
        .with_context(|| format!("plugin_jobs omits the wired agent: {after}"))?;
    anyhow::ensure!(
        wired["checks"][0]["check"] == "job_block_missing",
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
            "config",
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
        refusal.contains(&format!(
            "gents config tools explain --agent-did {}",
            fixture.agent
        )),
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
    let agent = initialized["default_agent_did"]
        .as_str()
        .context("init output missing default_agent_did")?
        .to_owned();
    let job_pack = scratch.join("job_plugin_fixture");
    copy_dir_all(&fixture_pack_dir("job_plugin_fixture"), &job_pack)?;
    run_cli_text(scratch, &["config", "pack", "build", dir_arg(&job_pack)])?;
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
            &["config", "pack", "show", dir_arg(&self.job_pack())],
        )?;
        Ok(shown["digest"]
            .as_str()
            .context("pack show has no digest")?
            .to_owned())
    }

    fn install_job_pack_args(&self, extra: &[&str]) -> Vec<String> {
        let mut args = vec![
            "config".to_owned(),
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
        gents_pack::pack::read_home_install(&fixture.home, JOB_PACK)?.is_some(),
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
    for needle in [wrong.as_str(), digest.as_str(), "gents config pack show"] {
        anyhow::ensure!(stderr.contains(needle), "{needle} missing from: {stderr}");
    }
    anyhow::ensure!(
        gents_pack::pack::read_home_install(&fixture.home, JOB_PACK)?.is_none()
            && gents_pack::plugin::store::list_records(&fixture.home)?.is_empty(),
        "a refused digest left an install behind"
    );
    Ok(())
}

/// The preview an install review shows reports the pack, what it would write
/// and the collections it declares, writes nothing, and its digest is the one
/// the reviewed install is then pinned to.
#[test]
fn pack_install_preview_reports_the_review_and_writes_nothing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("node");
    run_init_json(
        temp.path(),
        &["--node-name", "pack-review", "--home", dir_arg(&home)],
    )?;
    let pack = temp.path().join("documents_fixture");
    copy_dir_all(&fixture_pack_dir("documents_fixture"), &pack)?;
    let preview = run_cli_json(
        temp.path(),
        &[
            "config",
            "pack",
            "install",
            dir_arg(&pack),
            "--home",
            dir_arg(&home),
            "--registry",
            UNROUTABLE_REGISTRY,
            "--preview",
        ],
    )?;
    anyhow::ensure!(
        preview["coordinate"] == "fixture/documents_fixture"
            && preview["version"] == "1.0.0"
            && preview["kind"] == "documents"
            && preview["would_write"] == false
            && preview["collections"] == serde_json::json!(["FixtureJob"])
            && preview["authority"] == serde_json::json!([])
            && preview["declared_access"] == serde_json::json!([]),
        "the preview does not report the review: {preview}"
    );
    let documents = preview["documents"]
        .as_array()
        .context("the preview lists no documents")?;
    anyhow::ensure!(
        documents
            .iter()
            .any(|document| document["collection"] == "Agent"
                && document["id"] == "fixture-worker"
                && document["name"] == "Fixture worker"
                && document["content"]["display_name"] == "Fixture worker"),
        "the preview does not list the pack's agent and what it holds: {preview}"
    );
    anyhow::ensure!(
        gents_pack::pack::read_home_install(&home, "fixture/documents_fixture")?.is_none(),
        "a preview installed the pack"
    );
    let shown = run_cli_json(temp.path(), &["config", "pack", "show", dir_arg(&pack)])?;
    anyhow::ensure!(
        shown["digest"] == preview["digest"],
        "the previewed digest is not the pack's: {preview} {shown}"
    );
    Ok(())
}

#[test]
fn pack_install_preview_names_variables_without_their_values() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("node");
    run_init_json(
        temp.path(),
        &["--node-name", "pack-review", "--home", dir_arg(&home)],
    )?;
    let pack = documents_fixture_with_secret_variable(temp.path())?;
    let secret = "preview-secret-7f3a9c";
    let preview = run_cli_json_with_env(
        temp.path(),
        &[
            "config",
            "pack",
            "install",
            dir_arg(&pack),
            "--home",
            dir_arg(&home),
            "--registry",
            UNROUTABLE_REGISTRY,
            "--preview",
        ],
        &[("PACK_PREVIEW_SECRET", secret)],
    )?;
    assert_preview_hides_the_secret(&preview, secret)
}

/// `dependee_fixture` copied to `dir` with a second required slot, `checker`,
/// for a second agent, so the install can't choose either binding by itself.
fn two_slot_dependee(dir: &std::path::Path) -> Result<std::path::PathBuf> {
    let pack = dir.join("dependee_fixture");
    copy_dir_all(&fixture_pack_dir("dependee_fixture"), &pack)?;
    let manifest_path = pack.join("manifest.json");
    let mut manifest = read_json_file(&manifest_path)?;
    manifest["inference_slots"]
        .as_array_mut()
        .context("the fixture declares its slots")?
        .push(serde_json::json!({
            "name": "checker",
            "description": "Checks the dependee's work.",
            "agents": ["fixture-checker"],
        }));
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
    let config_path = pack.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    config["agents"]
        .as_array_mut()
        .context("the fixture has agents")?
        .push(serde_json::json!({
            "agent_slot": "fixture-checker",
            "display_name": "Fixture checker",
            "context_id": "fixture-dependee-context",
            "inference_profile_id": "gents:inference-slot:checker",
        }));
    std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
    Ok(pack)
}

/// The node's default inference profile: the default agent's DID with
/// `-profile`, as initialization names it.
fn default_profile_from_init(initialized: &serde_json::Value) -> Result<String> {
    let agent = initialized["default_agent_did"]
        .as_str()
        .or_else(|| initialized["init"]["default_agent_did"].as_str())
        .with_context(|| format!("init reports no default agent: {initialized}"))?;
    Ok(gents_documents::default_inference_profile_id_for_agent(
        agent,
    ))
}

fn slot<'a>(needs: &'a serde_json::Value, name: &str) -> Result<&'a serde_json::Value> {
    needs["slots"]
        .as_array()
        .and_then(|slots| slots.iter().find(|slot| slot["name"] == name))
        .with_context(|| format!("the preview does not report slot {name}: {needs}"))
}

/// A preview of a pack whose slots the install can't bind by itself reports
/// the slots, the node's profiles and which are still unbound, instead of
/// refusing; once they are bound it reports them bound. The install itself
/// still refuses the unbound slots.
#[test]
fn pack_install_preview_reports_the_slots_still_to_bind() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("node");
    let initialized = run_init_json(
        temp.path(),
        &["--node-name", "pack-slots", "--home", dir_arg(&home)],
    )?;
    let profile = default_profile_from_init(&initialized)?;
    let pack = two_slot_dependee(temp.path())?;
    let install = |extra: &[&str]| {
        let mut args = vec![
            "config",
            "pack",
            "install",
            dir_arg(&pack),
            "--home",
            dir_arg(&home),
            "--registry",
            UNROUTABLE_REGISTRY,
        ];
        args.extend_from_slice(extra);
        args.into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    let args = install(&["--preview"]);
    let preview = run_cli_json(
        temp.path(),
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    let needs = &preview["inference_needs"];
    anyhow::ensure!(
        preview["would_write"] == false
            && needs["unbound"] == serde_json::json!(["worker", "checker"]),
        "the preview does not report the unbound slots: {preview}"
    );
    anyhow::ensure!(
        *slot(needs, "checker")?
            == serde_json::json!({
                "name": "checker", "description": "Checks the dependee's work.",
                "optional": false, "packs": ["fixture/dependee_fixture"],
                "profile": null, "automatic": false,
            }),
        "{needs}"
    );
    anyhow::ensure!(
        needs["profiles"].as_array().is_some_and(|profiles| profiles
            .iter()
            .any(|option| option["profile_id"] == profile.as_str() && option["usable"] == true)),
        "the preview does not offer the node's profile: {needs}"
    );
    anyhow::ensure!(
        preview["documents"]
            .as_array()
            .is_some_and(|documents| documents.iter().any(|document| {
                document["collection"] == "Agent"
                    && document["id"] == "fixture-checker"
                    && document["name"] == "Fixture checker"
            })),
        "the preview does not list what it would write: {preview}"
    );

    let args = install(&[]);
    let refusal = run_cli_failure_stderr(
        temp.path(),
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    anyhow::ensure!(
        refusal.contains("requires explicit inference slot bindings for: worker, checker"),
        "the install did not refuse the unbound slots: {refusal}"
    );
    anyhow::ensure!(
        gents_pack::pack::read_home_install(&home, "fixture/dependee_fixture")?.is_none(),
        "the refused install installed the pack"
    );

    let worker = format!("worker={profile}");
    let checker = format!("checker={profile}");
    let args = install(&[
        "--preview",
        "--inference-slot",
        &worker,
        "--inference-slot",
        &checker,
    ]);
    let preview = run_cli_json(
        temp.path(),
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    let needs = &preview["inference_needs"];
    anyhow::ensure!(
        needs["unbound"] == serde_json::json!([])
            && slot(needs, "worker")?["profile"] == profile.as_str()
            && slot(needs, "checker")?["profile"] == profile.as_str()
            && slot(needs, "checker")?["automatic"] == false,
        "the preview does not report the chosen bindings: {preview}"
    );
    Ok(())
}

/// A preview reports the declared collections the node already has, which
/// the install shares, and none on a node that has none of them.
#[test]
fn pack_install_preview_reports_the_collections_it_shares() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("node");
    run_init_json(
        temp.path(),
        &["--node-name", "pack-shared", "--home", dir_arg(&home)],
    )?;
    let pack = temp.path().join("documents_fixture");
    copy_dir_all(&fixture_pack_dir("documents_fixture"), &pack)?;
    let install = |extra: &str| {
        let mut args = vec![
            "config",
            "pack",
            "install",
            dir_arg(&pack),
            "--home",
            dir_arg(&home),
            "--registry",
            UNROUTABLE_REGISTRY,
        ];
        if !extra.is_empty() {
            args.push(extra);
        }
        run_cli_json(temp.path(), &args)
    };
    let fresh = install("--preview")?;
    anyhow::ensure!(
        fresh["shared_collections"] == serde_json::json!([]),
        "a fresh node shares no collection: {fresh}"
    );
    install("")?;
    let again = install("--preview")?;
    anyhow::ensure!(
        again["shared_collections"] == serde_json::json!(["FixtureJob"]),
        "the preview does not report the collection the node has: {again}"
    );
    Ok(())
}

/// A pack with one required slot on a node with one usable profile is bound
/// by the install itself, and the preview says so.
#[test]
fn pack_install_preview_reports_the_binding_it_chooses_itself() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("node");
    let initialized = run_init_json(
        temp.path(),
        &["--node-name", "pack-auto-slot", "--home", dir_arg(&home)],
    )?;
    let profile = default_profile_from_init(&initialized)?;
    let preview = run_cli_json(
        temp.path(),
        &[
            "config",
            "pack",
            "install",
            dir_arg(&fixture_pack_dir("documents_fixture")),
            "--home",
            dir_arg(&home),
            "--registry",
            UNROUTABLE_REGISTRY,
            "--preview",
        ],
    )?;
    let needs = &preview["inference_needs"];
    let worker = slot(needs, "worker")?;
    anyhow::ensure!(
        needs["unbound"] == serde_json::json!([])
            && worker["profile"] == profile.as_str()
            && worker["automatic"] == true
            && worker["description"] == "Executes the fixture's one task.",
        "the preview does not report the binding it chose: {preview}"
    );
    Ok(())
}

/// A dependency's required slots are the install's too: a slot the pack binds
/// by itself but its dependency can't is still unbound, and binding each name
/// once binds it for both.
#[test]
fn pack_install_preview_reports_a_dependencys_slots_still_to_bind() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let dependee = two_slot_dependee(scratch.path())?;
    let setup = DependentHome::with_dependee("pack-dependency-slots", &dependee)?;
    let profile = setup
        .install_args
        .last()
        .and_then(|binding| binding.strip_prefix("worker="))
        .context("the install arguments end with the worker binding")?
        .to_owned();
    let unbound = &setup.install_args[..setup.install_args.len() - 2];
    let mut args: Vec<&str> = unbound.iter().map(String::as_str).collect();
    args.push("--preview");
    let preview = run_cli_json(setup.tempdir.path(), &args)?;
    let needs = &preview["inference_needs"];
    anyhow::ensure!(
        needs["unbound"] == serde_json::json!(["worker", "checker"])
            && slot(needs, "worker")?["packs"]
                == serde_json::json!(["fixture/dependent_fixture", "fixture/dependee_fixture"])
            && slot(needs, "worker")?["profile"] == serde_json::Value::Null
            && slot(needs, "checker")?["packs"] == serde_json::json!(["fixture/dependee_fixture"]),
        "the preview does not report the dependency's slots: {preview}"
    );
    let worker = format!("worker={profile}");
    let checker = format!("checker={profile}");
    args.extend(["--inference-slot", &worker, "--inference-slot", &checker]);
    let preview = run_cli_json(setup.tempdir.path(), &args)?;
    anyhow::ensure!(
        preview["inference_needs"]["unbound"] == serde_json::json!([]),
        "binding each slot once does not bind the dependency's: {preview}"
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
    let records = serde_json::to_value(gents_pack::plugin::store::list_records(&fixture.home)?)?;
    let tools = fixture.agent_tools()?;

    let stderr = fixture.install_job_pack_failure(&["--wire", &fixture.agent])?;
    anyhow::ensure!(
        stderr.contains("--grant-authority"),
        "the refusal does not name --grant-authority: {stderr}"
    );
    anyhow::ensure!(
        serde_json::to_value(gents_pack::plugin::store::list_records(&fixture.home)?)? == records
            && fixture.agent_tools()? == tools,
        "a refused install changed the plugin store or the agent's Tools"
    );
    Ok(())
}

/// `wire_sources` resolves the CLI client key from the process environment,
/// so it runs in a child of this test binary whose `HOME` is the fixture's.
#[test]
fn wire_sources_lists_job_plugins_and_target_packs() -> Result<()> {
    let fixture = wire_fixture(true)?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "cli_pack::wire_sources_in_fixture_home",
            "--exact",
            "--include-ignored",
            "--nocapture",
        ])
        .env("HOME", fixture.tempdir.path())
        .env_remove(gents_desktop_core::client::DESKTOP_HOME_ENV)
        .env(WIRE_SOURCES_HOME_ENV, &fixture.home)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "wire_sources child failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

const WIRE_SOURCES_HOME_ENV: &str = "GENTS_TEST_WIRE_SOURCES_HOME";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by wire_sources_lists_job_plugins_and_target_packs in the fixture's HOME"]
async fn wire_sources_in_fixture_home() -> Result<()> {
    let home = std::path::PathBuf::from(
        std::env::var_os(WIRE_SOURCES_HOME_ENV).context("fixture home is not set")?,
    );
    let sources = gents_server::packs::wire_sources(home).await?;
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

const STARTER_HOME_ENV: &str = "GENTS_TEST_STARTER_HOME";
const STARTER_STEP_ENV: &str = "GENTS_TEST_STARTER_STEP";

/// A copy of the job fixture pack renamed to the starter pack `gents/<name>`,
/// shipping the plugin `gents/<plugin>` (which requests an environment
/// variable) and one skill, admitted to `home`'s pack store.
fn admit_starter_fixture(
    scratch: &std::path::Path,
    home: &std::path::Path,
    name: &str,
    plugin: &str,
    job: bool,
) -> Result<()> {
    let dir = scratch.join(name);
    copy_dir_all(&fixture_pack_dir("job_plugin_fixture"), &dir)?;
    std::fs::remove_file(dir.join("plugins/job_fixture.afb")).ok();
    if plugin != "job_fixture" {
        std::fs::rename(
            dir.join("plugins/job_fixture"),
            dir.join("plugins").join(plugin),
        )?;
        for file in ["Cargo.toml", "Cargo.lock"] {
            let path = dir.join("plugins").join(plugin).join(file);
            let text = std::fs::read_to_string(&path)?;
            std::fs::write(&path, text.replace("job_fixture", plugin))?;
        }
    }
    let manifest_path = dir.join("manifest.json");
    let mut manifest = read_json_file(&manifest_path)?;
    manifest["name"] = name.into();
    manifest["namespace"] = "gents".into();
    manifest["assets"] = serde_json::json!([
        "README.md",
        "pack_config.json",
        format!("plugins/{plugin}.afb"),
    ]);
    manifest.as_object_mut().unwrap().remove("schemas");
    let entry = &mut manifest["plugins"][0];
    entry["name"] = plugin.into();
    entry["artifact"] = format!("plugins/{plugin}.afb").into();
    entry["source"] = format!("plugins/{plugin}").into();
    entry["job"] = job.into();
    entry.as_object_mut().unwrap().remove("data");
    entry["manifold"] = serde_json::json!({
        "fs": "None", "net": "None", "crypto": false, "child_process": false,
        "env": {"AllowList": ["STARTER_FIXTURE"]},
    });
    write_json_file(&manifest_path, &manifest)?;
    let mut selection = serde_json::json!({"plugin": format!("gents/{plugin}")});
    let mut config = serde_json::json!({
        "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
        "skills": [{
            "skill_id": format!("starter-{name}"), "node_did": "${GENTS_PACK_NODE_DID}",
            "name": name, "description": "A starter skill.", "instructions": "Use it.",
        }],
    });
    if job {
        let target = format!("wf-{name}");
        selection["job"] =
            serde_json::json!({"agent_targets": [target], "policy": {"dynamic": "check"}});
        config["agent_targets"] = serde_json::json!([wire_target(&target)]);
        config["tools"] = serde_json::json!([{
            "tools_id": format!("{name}-caller"),
            "display_name": "Starter fixture caller",
            "integrations": {"plugins": [selection]},
        }]);
    }
    edit_pack_config(&dir, |existing| *existing = config)?;
    run_cli_text(scratch, &["config", "pack", "build", dir_arg(&dir)])?;
    gents_pack::pack::local::open(&gents_pack::pack::local::LocalName::Path(&dir), home)
        .with_context(|| format!("admitting {name} to the home's pack store"))?;
    Ok(())
}

fn starter_step(fixture: &std::path::Path, home: &std::path::Path, step: &str) -> Result<()> {
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "cli_pack::starter_packs_in_fixture_home",
            "--exact",
            "--include-ignored",
            "--nocapture",
        ])
        .env("HOME", fixture)
        .env_remove(gents_desktop_core::client::DESKTOP_HOME_ENV)
        .env(STARTER_HOME_ENV, home)
        .env(STARTER_STEP_ENV, step)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "starter step {step} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// What the desktop's starter packs card does, against a real runtime. The
/// list is read while the runtime serves the home. An approval installs
/// through the home's own store, so it is refused, writing nothing, while
/// the runtime runs, and the desktop restarts the node around it: here the
/// runtime is stopped, the packs approved (twice: approving again changes
/// nothing) and the runtime started again, where the Engineer's surface has
/// the packs' plugin tools and its Context their skills.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_packs_are_approved_around_a_running_runtime() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let scratch = tempdir.path();
    let home = scratch.join("agent-home");
    let home_arg = dir_arg(&home);
    let initialized = run_init_json(
        scratch,
        &[
            "--engineer",
            "--no-default-packs",
            "--model-name",
            "test-model",
            "--inference-url",
            "http://127.0.0.1:9/v1",
            "--home",
            home_arg,
        ],
    )?;
    let node_did = node_did_from_init(&initialized)?;
    let engineer = initialized["default_agent_did"]
        .as_str()
        .context("init output missing default_agent_did")?
        .to_owned();
    admit_starter_fixture(scratch, &home, "dynamic_workflows", "job_fixture", true)?;
    admit_starter_fixture(scratch, &home, "script", "script", false)?;

    let serve = |port: u16| spawn_server_with_ready_json(scratch, port, &["--home", home_arg], &[]);
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (mut server, _) = serve(port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &node_did, std::time::Duration::from_secs(60)).await?;
    wait_for_runtime_state_graphql(&home, &graphql, std::time::Duration::from_secs(30)).await?;
    starter_step(scratch, &home, "running")?;
    drop(server);

    starter_step(scratch, &home, "stopped")?;

    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (mut server, _) = serve(port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &node_did, std::time::Duration::from_secs(60)).await?;
    wait_for_runtime_state_graphql(&home, &graphql, std::time::Duration::from_secs(30)).await?;
    starter_step(scratch, &home, "restarted")?;
    let explained = run_cli_json(
        scratch,
        &[
            "config",
            "tools",
            "explain",
            "--agent-did",
            &engineer,
            "--home",
            home_arg,
        ],
    )?;
    let surface = explained["agents"]
        .as_array()
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent["agent_did"] == engineer.as_str())
        })
        .and_then(|agent| agent.pointer("/surface/tool_names"))
        .and_then(serde_json::Value::as_array)
        .with_context(|| format!("the Engineer has no explained surface: {explained}"))?;
    for tool in ["job_fixture", "script"] {
        anyhow::ensure!(
            surface.iter().any(|name| name == tool),
            "the running Engineer's surface lacks the plugin tool {tool}: {explained}"
        );
    }
    let tools = run_cli_json(
        scratch,
        &[
            "config",
            "tools",
            "show",
            &format!("{engineer}-tools"),
            "--home",
            home_arg,
        ],
    )?;
    let selected = |plugin: &str| {
        tools["integrations"]["plugins"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| entry["plugin"] == plugin)
            .collect::<Vec<_>>()
    };
    let workflow = selected("gents/job_fixture");
    anyhow::ensure!(
        workflow.len() == 1 && workflow[0]["job"]["policy"]["dynamic"] == "run",
        "approving gents/dynamic_workflows did not set its job block to run, once: {tools}"
    );
    let script = selected("gents/script");
    anyhow::ensure!(
        script.len() == 1 && script[0]["tool_calls"] == true,
        "approving gents/script did not wire it with tool_calls, once: {tools}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by starter_packs_are_approved_around_a_running_runtime in the fixture's HOME"]
async fn starter_packs_in_fixture_home() -> Result<()> {
    let home = std::path::PathBuf::from(
        std::env::var_os(STARTER_HOME_ENV).context("fixture home is not set")?,
    );
    let step = std::env::var(STARTER_STEP_ENV).context("fixture step is not set")?;
    let registry = Some(UNROUTABLE_REGISTRY.to_owned());
    let listed = gents_server::packs::default_packs(home.clone(), registry.clone()).await?;
    let state = |pack: &str| {
        listed["packs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|listed| listed["pack"] == pack)
            .map(|listed| listed["state"].clone())
    };
    let fixtures = ["gents/dynamic_workflows", "gents/script"];
    let approved = || {
        fixtures
            .iter()
            .map(|pack| {
                let digest = listed["packs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|listed| listed["pack"] == *pack)
                    .and_then(|listed| listed["digest"].as_str())
                    .with_context(|| format!("{pack} lists no digest: {listed}"))?;
                Ok(((*pack).to_owned(), digest.to_owned()))
            })
            .collect::<Result<Vec<_>>>()
    };
    anyhow::ensure!(
        listed["packs"].as_array().map(Vec::len) == Some(6)
            && state("gents/web") == Some("unavailable".into()),
        "{step}: every starter pack is listed, and one nothing publishes is unavailable: {listed}"
    );
    match step.as_str() {
        "running" => {
            anyhow::ensure!(
                fixtures
                    .iter()
                    .all(|pack| state(pack) == Some("needs_approval".into())),
                "the running node's packs wait on an approval: {listed}"
            );
            let refused =
                gents_server::packs::approve_default_packs(home.clone(), approved()?, registry)
                    .await
                    .expect_err("an approval while the runtime serves the home");
            anyhow::ensure!(
                format!("{refused:#}").contains("a runtime is serving this home"),
                "{refused:#}"
            );
            anyhow::ensure!(
                gents_pack::pack::read_home_install(&home, "gents/script")?.is_none()
                    && gents_pack::plugin::store::list_records(&home)?.is_empty(),
                "a refused approval installed something"
            );
        }
        "stopped" => {
            for _ in 0..2 {
                let report = gents_server::packs::approve_default_packs(
                    home.clone(),
                    approved()?,
                    registry.clone(),
                )
                .await?;
                anyhow::ensure!(
                    report["failed"] == serde_json::json!([])
                        && report["installed"].as_array().map(Vec::len) == Some(2),
                    "the approval did not install both packs: {report}"
                );
                anyhow::ensure!(
                    report["installed"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .all(|pack| pack["skills"].as_array().map(Vec::len) == Some(1)
                            && pack["granted_permissions"].as_array().map(Vec::len) == Some(1)),
                    "each pack's skill is attached and its permission granted: {report}"
                );
            }
        }
        "restarted" => anyhow::ensure!(
            fixtures
                .iter()
                .all(|pack| state(pack) == Some("installed".into())),
            "the restarted node lists the approved packs as installed: {listed}"
        ),
        other => anyhow::bail!("unknown step {other}"),
    }
    Ok(())
}
