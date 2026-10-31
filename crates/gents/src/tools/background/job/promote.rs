//! Promotion: a recorded dynamic workflow run becomes a workflow pack in the
//! gents home. The user's script is never edited; the pack's `main.js` is the
//! script followed by a wrapper that pins step 0 the way the dynamic run did.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents_protocol::job::{JobCeiling, JobGuestPin, JobOrigin, JobState, JobView};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::pack::authoring::scaffold::{scaffold, ScaffoldInput};
use crate::pack::authoring::template::Template;
use crate::plugin::{PluginBudget, PluginVerdict};
use crate::store::config_client::ConfigAccess;
use crate::tools::background::job::view::{load_job_view, JobKey};

/// The namespace every promoted pack lives under.
const NAMESPACE: &str = "gents";

/// A terminal dynamic run with everything promotion reads from it.
#[derive(Debug, Clone)]
pub struct DynamicRun {
    pub view: JobView,
    pub script: String,
    pub args: Option<Value>,
    /// The run's spawn targets plus the caller job block's `policy.dynamic_targets`.
    pub targets: Vec<String>,
    /// The installed packs that ship `targets`; empty when some target is
    /// shipped by no installed pack.
    pub owners: Vec<String>,
    /// The ceiling recorded on the run's pin.
    pub ceiling: JobCeiling,
    pub caller_agent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoteRefusal {
    /// The run is not `Completed`: still live, or ended failed, cancelled
    /// or timed out.
    NotCompleted(JobState),
    NotDynamic,
    SourceDigestMismatch {
        pinned: String,
        actual: String,
    },
    NoTargetOwner,
    BadName(String),
    NameTaken(String),
    /// The target agent has no Tools document to wire the tool into.
    NoAgentTools(String),
    /// The run or its caller could not be read.
    Unreadable(String),
}

impl PromoteRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotCompleted(_) => "promote_not_completed",
            Self::NotDynamic => "promote_not_dynamic",
            Self::SourceDigestMismatch { .. } => "promote_source_digest_mismatch",
            Self::NoTargetOwner => "promote_no_target_owner",
            Self::BadName(_) => "promote_bad_name",
            Self::NameTaken(_) => "promote_name_taken",
            Self::NoAgentTools(_) => "promote_no_agent_tools",
            Self::Unreadable(_) => "promote_unreadable",
        }
    }
}

impl std::fmt::Display for PromoteRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCompleted(state) => write!(
                f,
                "the run is {state:?}, not completed; only a completed run can be promoted"
            ),
            Self::NotDynamic => f.write_str("the run is not a dynamic script run, so there is no script to promote"),
            Self::SourceDigestMismatch { pinned, actual } => write!(
                f,
                "the recorded script hashes to {actual}, not the pinned {pinned}, so it is not the script that ran"
            ),
            Self::NoTargetOwner => f.write_str(
                "a target of the run is shipped by no installed pack, so the promoted pack cannot depend on its owner",
            ),
            Self::BadName(name) => write!(
                f,
                "{name:?} is not a pack name; use lowercase letters, digits and underscores, starting with a letter"
            ),
            Self::NameTaken(name) => write!(f, "gents/{name} is already a pack or plugin; choose another name"),
            Self::NoAgentTools(agent) => write!(
                f,
                "agent {agent:?} has no Tools document to wire the tool into; pass an existing agent with --agent"
            ),
            Self::Unreadable(message) => write!(f, "the run could not be read: {message}"),
        }
    }
}

impl std::error::Error for PromoteRefusal {}

/// The run's `script_id`: sha256 of the exact script bytes the executor runs.
pub fn script_digest(script: &str) -> String {
    format!("{:x}", Sha256::digest(script.as_bytes()))
}

/// Reads the run at `key` as the node's operator and refuses one that is not a
/// completed dynamic run whose recorded script is the one it pinned.
pub async fn load_dynamic_run(
    access: &ConfigAccess,
    key: JobKey,
) -> Result<DynamicRun, PromoteRefusal> {
    let unreadable = |error: anyhow::Error| PromoteRefusal::Unreadable(format!("{error:#}"));
    let view = load_job_view(access, None, key)
        .await
        .map_err(|error| PromoteRefusal::Unreadable(error.to_string()))?;
    gate(&view)?;
    let row = crate::tools::background::job::driver::read_step_row(access, &view.row_doc_id)
        .await
        .map_err(unreadable)?;
    let script = row
        .call
        .script
        .as_deref()
        .ok_or(PromoteRefusal::NotDynamic)?
        .to_owned();
    let actual = script_digest(&script);
    if actual != view.pin.script_id {
        return Err(PromoteRefusal::SourceDigestMismatch {
            pinned: view.pin.script_id.clone(),
            actual,
        });
    }
    let args = row
        .call
        .args
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| PromoteRefusal::Unreadable(format!("the run's args: {error}")))?;
    let ceiling = row
        .pin
        .ok_or_else(|| PromoteRefusal::Unreadable("the run has no job_pin".into()))?
        .ceiling_at_pin;
    let session = crate::session::origin::load_session(
        access,
        &view.session_id,
        view.scope.requester_did.as_deref(),
    )
    .await
    .map_err(unreadable)?
    .ok_or_else(|| {
        PromoteRefusal::Unreadable(format!("session {} does not exist", view.session_id))
    })?;
    let owner = view.scope.node_did.clone();
    let agent_id = session.agent_id.clone();
    let tools = access
        .transact_readonly("plugin_job.promote.caller_tools", move |txn| {
            let (owner, agent_id) = (owner.clone(), agent_id.clone());
            Box::pin(async move {
                crate::documents::load_agent_tools_in_txn(txn, &owner, &agent_id).await
            })
        })
        .await
        .map_err(unreadable)?;
    let policy = tools
        .and_then(|tools| tools.integrations)
        .and_then(|integrations| integrations.plugins)
        .into_iter()
        .flatten()
        .find(|plugin| plugin.plugin == view.pin.plugin_ref)
        .and_then(|plugin| plugin.job)
        .map(|job| job.policy)
        .unwrap_or_default();
    let children: Vec<String> = view
        .children
        .iter()
        .map(|child| child.target.clone())
        .collect();
    let targets = effective_targets(&children, &policy.dynamic_targets, &ceiling.targets);
    let node = &view.scope.node_did;
    let mut shipped = Vec::new();
    for pack in crate::pack::list_installed_packs(access, node)
        .await
        .map_err(unreadable)?
    {
        let ids = crate::pack::installed_pack_documents(access, node, &pack.coordinate)
            .await
            .map_err(unreadable)?
            .into_iter()
            .filter(|(collection, _)| {
                *collection == crate::store::collection::Collection::AgentTarget
            })
            .map(|(_, id)| id)
            .collect();
        shipped.push((pack.coordinate, ids));
    }
    let owners = target_owners(&targets, &shipped).unwrap_or_default();
    Ok(DynamicRun {
        view,
        script,
        args,
        targets,
        owners,
        ceiling,
        caller_agent_id: session.agent_id,
    })
}

/// Refuses a run that did not complete or did not run a dynamic script.
pub fn gate(view: &JobView) -> Result<(), PromoteRefusal> {
    if view.state != JobState::Completed {
        return Err(PromoteRefusal::NotCompleted(view.state));
    }
    if view.pin.origin != JobOrigin::Dynamic {
        return Err(PromoteRefusal::NotDynamic);
    }
    Ok(())
}

/// The run's spawn targets plus `dynamic_targets`, or the pin ceiling's
/// targets when both are empty (a run that spawned nothing under a block with
/// no `dynamic_targets`). The pin ceiling always carries the caller's wired
/// targets, so the result is never empty for a run that pinned.
pub fn effective_targets(
    children: &[String],
    dynamic_targets: &[String],
    ceiling: &[String],
) -> Vec<String> {
    let union: BTreeSet<&String> = children.iter().chain(dynamic_targets).collect();
    let chosen = if union.is_empty() {
        ceiling.iter().collect()
    } else {
        union
    };
    chosen.into_iter().cloned().collect()
}

/// The packs in `shipped` (pack, the AgentTarget ids it ships) that ship
/// `targets`, or `None` when some target is shipped by no pack.
pub fn target_owners(targets: &[String], shipped: &[(String, Vec<String>)]) -> Option<Vec<String>> {
    let mut owners = BTreeSet::new();
    for target in targets {
        let packs: Vec<&String> = shipped
            .iter()
            .filter(|(_, ids)| ids.contains(target))
            .map(|(pack, _)| pack)
            .collect();
        if packs.is_empty() {
            return None;
        }
        owners.extend(packs.into_iter().cloned());
    }
    Some(owners.into_iter().collect())
}

/// `name` is `^[a-z][a-z0-9_]*$` and not yet `gents/<name>` as an installed
/// pack or plugin. A leftover `packs/promoted/<name>` directory does not take it.
pub fn validate_name(home: &Path, name: &str) -> Result<(), PromoteRefusal> {
    if !crate::pack::is_valid_pack_name(name) {
        return Err(PromoteRefusal::BadName(name.to_owned()));
    }
    let coordinate = format!("{NAMESPACE}/{name}");
    let taken = !matches!(
        crate::pack::home_install::read_home_install(home, &coordinate),
        Ok(None)
    ) || home
        .join(crate::store::local::home::PLUGINS_DIR_NAME)
        .join("installed")
        .join(NAMESPACE)
        .join(format!("{name}.json"))
        .exists();
    if taken {
        return Err(PromoteRefusal::NameTaken(name.to_owned()));
    }
    Ok(())
}

/// A generated pack: its directory, plugin coordinate, the packs it depends
/// on for its targets and its step-0 fixture.
#[derive(Debug, Clone)]
pub struct GeneratedPack {
    pub dir: PathBuf,
    /// `gents/<name>`.
    pub plugin: String,
    pub owners: Vec<String>,
    pub fixture: Value,
}

/// Writes a JavaScript workflow pack at `dir` whose plugin runs `run.script`
/// unchanged, with a caller Tools template over the run's targets and a step-0
/// fixture recorded from the wrapped script.
pub fn generate_pack(dir: &Path, name: &str, run: &DynamicRun) -> Result<GeneratedPack> {
    if run.owners.is_empty() {
        return Err(PromoteRefusal::NoTargetOwner.into());
    }
    let report = scaffold(
        dir,
        name,
        &ScaffoldInput {
            namespace: NAMESPACE.into(),
            template: Some(Template::Workflow),
            kind: None,
            language: Some("js".into()),
            ux: false,
        },
    )?;
    let plugin = format!("{NAMESPACE}/{name}");
    let plugin_dir = format!("plugins/{name}");
    let script_id = format!("{:x}", Sha256::digest(run.script.as_bytes()));
    let main = wrap_script(
        &run.script,
        &JobGuestPin {
            source_digest: format!("sha256:{script_id}"),
            script_id,
            agent_budget: run.view.pin.agent_budget,
            handle: name.to_owned(),
        },
    );

    let input = json!({
        "pin": null,
        "args": run.args.clone().unwrap_or_else(|| json!({})),
        "agent_budget": run.view.pin.agent_budget,
        "children": {},
        "ceiling": {
            "targets": run.targets,
            "max_live": run.ceiling.max_live,
            "remaining_spawns": run.ceiling.max_spawns,
            "max_messages_per_slot": run.ceiling.max_messages_per_slot,
            "result_bytes": run.ceiling.result_bytes,
        },
    });
    // `pack test` checks this fixture against the compiled artifact, which
    // keeps the wrapper's pin; a script run would stamp the wrapper's digest.
    let outcome =
        crate::plugin::script_engine::eval(&main, &plugin, &input, &PluginBudget::default())?;
    anyhow::ensure!(
        outcome.verdict == PluginVerdict::Success,
        "the script fails on step 0: {}",
        outcome.diagnostics
    );
    let fixture = json!({"input": input, "expect": outcome.output});

    let kebab = name.replace('_', "-");
    let config = json!({
        "node": {},
        "tools": [{
            "tools_id": format!("{kebab}-caller"),
            "display_name": format!("{name} caller: the workflow tool over its targets"),
            "integrations": {"plugins": [{
                "plugin": plugin,
                "job": {
                    "agent_targets": run.targets,
                    "max_live": run.ceiling.max_live,
                    "max_spawns": run.ceiling.max_spawns,
                    "max_messages_per_slot": run.ceiling.max_messages_per_slot,
                    "result_bytes": run.ceiling.result_bytes,
                    "max_wall_secs": run.ceiling.max_wall_secs,
                    "policy": {"dynamic": "off"},
                },
            }]},
        }],
    });
    let args_shape = serde_json::to_string(&input["args"])?;
    let tool = format!(
        "# {name}\n\n\
         Run the {name} workflow as a background job, promoted from dynamic run `{address}`. \
         The call returns a receipt at once; the result arrives later as a message. \
         End your turn after the call.\n\n\
         ## Input\n\n\
         - `args`: the workflow's arguments, shaped like the recorded run's `{args_shape}`.\n\
         - `agent_budget`: how many child agents the run may start (the recorded run used {budget}).\n\n\
         ## Output\n\n\
         A receipt `{{address, handle, status}}`; later the workflow's result.\n",
        address = run.view.address,
        budget = run.view.pin.agent_budget,
    );

    let fixture_path = format!("{plugin_dir}/tests/00_step0.json");
    let scaffolded_fixture = format!("{plugin_dir}/tests/00_step0_spawns_worker.json");
    let write = |path: &str, contents: String| {
        let target = dir.join(path);
        std::fs::write(&target, contents).with_context(|| format!("writing {}", target.display()))
    };
    write(&format!("{plugin_dir}/source/main.js"), main)?;
    write(&format!("{plugin_dir}/TOOL.md"), tool)?;
    write(
        &fixture_path,
        serde_json::to_string_pretty(&fixture)? + "\n",
    )?;
    write(
        "pack_config.json",
        serde_json::to_string_pretty(&config)? + "\n",
    )?;
    std::fs::remove_file(dir.join(&scaffolded_fixture))
        .with_context(|| format!("removing {scaffolded_fixture}"))?;
    std::fs::remove_dir_all(dir.join("agents")).context("removing agents/")?;

    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).context("reading manifest.json")?,
    )?;
    manifest["description"] = json!(format!(
        "The {name} workflow, promoted from dynamic run {}.",
        run.view.address
    ));
    manifest["dependencies"] = json!(run.owners);
    manifest["inference_slots"] = json!([]);
    manifest["plugins"][0]["input_schema"]["properties"]["args"] = json!({
        "type": "object",
        "description": format!("the workflow's arguments, shaped like {args_shape}"),
    });
    let mut assets: Vec<String> = report
        .files
        .into_iter()
        .filter(|path| {
            path != "manifest.json" && path != &scaffolded_fixture && !path.starts_with("agents/")
        })
        .chain([fixture_path, format!("plugins/{name}.afb")])
        .collect();
    assets.sort();
    manifest["assets"] = json!(assets);
    write(
        "manifest.json",
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;

    Ok(GeneratedPack {
        dir: dir.to_path_buf(),
        plugin,
        owners: run.owners.clone(),
        fixture,
    })
}

/// `script` followed by a wrapper that gives a spawning step-0 output without a
/// pin the given `pin`, as the host did for the dynamic run. The caller's
/// `input.agent_budget` wins over `pin.agent_budget`, the recorded default.
pub fn wrap_script(script: &str, pin: &JobGuestPin) -> String {
    let quote = |text: &str| serde_json::to_string(text).expect("a string serializes");
    format!(
        "{script}\n\
         const __gents_step = module.exports;\n\
         module.exports = (input) => {{\n  \
           const out = __gents_step(input);\n  \
           if (!input || input.pin || !out || !out.effects || !Array.isArray(out.effects.spawn) || out.effects.spawn.length === 0 || out.effects.pin) return out;\n  \
           out.effects.pin = {{ script_id: {script_id}, source_digest: {source_digest}, agent_budget: input.agent_budget == null ? {agent_budget} : input.agent_budget, handle: {handle} }};\n  \
           return out;\n\
         }};\n",
        script_id = quote(&pin.script_id),
        source_digest = quote(&pin.source_digest),
        agent_budget = pin.agent_budget,
        handle = quote(&pin.handle),
    )
}

/// What `promotion.json` beside a promoted pack records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionRecord {
    pub name: String,
    pub plugin: String,
    pub script_id: String,
    pub address: String,
    pub session_id: String,
    /// RFC 3339.
    pub promoted_at: String,
}

pub fn promoted_dir(home: &Path) -> PathBuf {
    home.join(crate::store::local::home::PACKS_DIR_NAME)
        .join("promoted")
}

pub fn write_promotion(home: &Path, record: &PromotionRecord) -> Result<()> {
    let dir = promoted_dir(home).join(&record.name);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("promotion.json");
    std::fs::write(&path, serde_json::to_vec_pretty(record)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// Every promotion record in the home whose `gents/<name>` pack is still
/// installed, newest first; `pack remove` leaves the record behind.
pub fn list_promotions(home: &Path) -> Result<Vec<PromotionRecord>> {
    let dir = promoted_dir(home);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry?.path().join("promotion.json");
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let record = serde_json::from_slice::<PromotionRecord>(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        let coordinate = format!("{NAMESPACE}/{}", record.name);
        // An unreadable install record still takes the name (`validate_name`).
        if !matches!(
            crate::pack::home_install::read_home_install(home, &coordinate),
            Ok(None)
        ) {
            records.push(record);
        }
    }
    records.sort_by(|a, b| b.promoted_at.cmp(&a.promoted_at));
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::job::{JobPinSummary, JobScope};

    const SPAWNING: &str = r#"module.exports = (i) => i.pin ? {effects:{}} : {effects:{spawn:[{slot:"a",attempt:0,target:"t",prompt:"p",title:"a"}]}};"#;

    fn guest_pin() -> JobGuestPin {
        JobGuestPin {
            script_id: "abc123".into(),
            source_digest: "sha256:abc123".into(),
            agent_budget: 8,
            handle: "rome".into(),
        }
    }

    fn run_js(source: String, input: Value) -> Value {
        let outcome = crate::plugin::script_engine::eval(
            &source,
            "gents/test",
            &input,
            &PluginBudget::default(),
        )
        .unwrap();
        assert_eq!(
            outcome.verdict,
            PluginVerdict::Success,
            "{}",
            outcome.diagnostics
        );
        outcome.output
    }

    #[test]
    fn wrap_script_adds_the_pin_to_a_spawning_step0_only() {
        let wrapped = wrap_script(SPAWNING, &guest_pin());
        let step0 = run_js(wrapped.clone(), json!({"pin": null, "args": {}}));
        assert_eq!(
            step0["effects"]["pin"],
            json!({"script_id": "abc123", "source_digest": "sha256:abc123", "agent_budget": 8, "handle": "rome"})
        );
        assert_eq!(step0["effects"]["spawn"][0]["slot"], "a");
        let later = run_js(wrapped, json!({"pin": {"script_id": "abc123"}, "args": {}}));
        assert_eq!(later, json!({"effects": {}}));
    }

    #[test]
    fn wrap_script_pins_the_callers_agent_budget_over_the_recorded_one() {
        let wrapped = wrap_script(SPAWNING, &guest_pin());
        let given = run_js(wrapped.clone(), json!({"pin": null, "agent_budget": 2}));
        assert_eq!(given["effects"]["pin"]["agent_budget"], 2);
        let default = run_js(wrapped, json!({"pin": null}));
        assert_eq!(default["effects"]["pin"]["agent_budget"], 8);
    }

    #[test]
    fn wrap_script_captures_module_exports_before_the_harness_names() {
        let script = format!("const __fn = 1;\n{SPAWNING}");
        let output = run_js(wrap_script(&script, &guest_pin()), json!({"pin": null}));
        assert_eq!(output["effects"]["spawn"][0]["target"], "t");
        assert_eq!(output["effects"]["pin"]["handle"], "rome");
    }

    #[test]
    fn script_digest_preserves_leading_trailing_whitespace_and_newlines() {
        let source = " \nmodule.exports = (i) => i;\n \t";
        assert_eq!(script_digest(source), sha(source));
        assert_ne!(script_digest(source), sha(source.trim()));
    }

    #[test]
    fn the_recorded_script_is_promoted_byte_for_byte() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("rome");
        let mut run = run(vec!["gents/dynamic_workflows".into()]);
        run.script = format!("{}\n", run.script);
        run.view.pin.script_id = sha(&run.script);
        let pack = generate_pack(&dir, "rome", &run).unwrap();
        let main = std::fs::read_to_string(dir.join("plugins/rome/source/main.js")).unwrap();
        assert!(main.starts_with(&run.script), "the newline is kept");
        assert_eq!(
            pack.fixture["expect"]["effects"]["pin"]["script_id"],
            sha(&run.script)
        );
    }

    fn sha(text: &str) -> String {
        format!("{:x}", Sha256::digest(text.as_bytes()))
    }

    fn run(owners: Vec<String>) -> DynamicRun {
        let script = SPAWNING.replace("target:\"t\"", "target:\"wf-read-only\"");
        DynamicRun {
            view: JobView {
                address: "session-1:call_1".into(),
                row_doc_id: "row-1".into(),
                session_id: "session-1".into(),
                tool_call_id: "call_1".into(),
                scope: JobScope {
                    session_id: "session-1".into(),
                    request_id: "request-1".into(),
                    node_did: "did:key:z6Mknode".into(),
                    requester_did: None,
                },
                handle: "rome".into(),
                state: JobState::Completed,
                outcome: None,
                pin: JobPinSummary {
                    script_id: sha(&script),
                    origin: JobOrigin::Dynamic,
                    engine_digest: "engine".into(),
                    plugin_ref: "gents/dynamic_workflows".into(),
                    agent_budget: 8,
                    cwd: "/work".into(),
                    chain_root_doc_id: "row-1".into(),
                    resume_of: None,
                },
                progress: None,
                children: vec![],
                created_at: 0,
                adopted_at: 0,
                completed_at: Some(1),
            },
            script,
            args: Some(json!({"sides": ["a", "b"]})),
            targets: vec!["wf-read-only".into()],
            owners,
            ceiling: JobCeiling {
                targets: vec!["wf-read-only".into()],
                max_live: 4,
                max_spawns: 12,
                max_messages_per_slot: 3,
                result_bytes: 65536,
                max_wall_secs: 1800,
            },
            caller_agent_id: "caller".into(),
        }
    }

    fn read_json(path: &std::path::Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn generate_pack_writes_a_js_workflow_pack_from_a_run() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("rome");
        let run = run(vec!["gents/dynamic_workflows".into()]);
        let pack = generate_pack(&dir, "rome", &run).unwrap();
        assert_eq!(pack.dir, dir);
        assert_eq!(pack.plugin, "gents/rome");
        assert_eq!(pack.owners, vec!["gents/dynamic_workflows".to_owned()]);

        let manifest = read_json(&dir.join("manifest.json"));
        assert_eq!(manifest["plugins"][0]["language"], "js");
        assert_eq!(manifest["plugins"][0]["job"], true);
        assert!(manifest["dependencies"]
            .as_array()
            .unwrap()
            .contains(&json!("gents/dynamic_workflows")));
        assert_eq!(manifest["inference_slots"], json!([]));
        assert!(!dir.join("agents").exists());
        for asset in manifest["assets"].as_array().unwrap() {
            let asset = asset.as_str().unwrap();
            assert!(
                asset.ends_with(".afb") || dir.join(asset).exists(),
                "{asset}"
            );
        }

        let config = read_json(&dir.join("pack_config.json"));
        let tools = config["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["tools_id"], "rome-caller");
        let plugin = &tools[0]["integrations"]["plugins"][0];
        assert_eq!(plugin["plugin"], "gents/rome");
        assert_eq!(plugin["job"]["agent_targets"], json!(["wf-read-only"]));
        assert_eq!(plugin["job"]["policy"]["dynamic"], "off");
        for absent in ["agents", "contexts", "agent_targets"] {
            assert!(config.get(absent).is_none(), "{absent}");
        }

        let main = std::fs::read_to_string(dir.join("plugins/rome/source/main.js")).unwrap();
        assert!(main.starts_with(&run.script));

        let fixture = read_json(&dir.join("plugins/rome/tests/00_step0.json"));
        assert!(!dir
            .join("plugins/rome/tests/00_step0_spawns_worker.json")
            .exists());
        assert_eq!(fixture["input"]["pin"], Value::Null);
        assert_eq!(fixture["input"]["args"], json!({"sides": ["a", "b"]}));
        assert_eq!(
            fixture["input"]["ceiling"]["targets"],
            json!(["wf-read-only"])
        );
        assert_eq!(fixture, pack.fixture);
        let expect = run_js(main, fixture["input"].clone());
        assert_eq!(fixture["expect"], expect);
        assert_eq!(
            expect["effects"]["pin"]["script_id"],
            json!(run.view.pin.script_id)
        );

        let tool = std::fs::read_to_string(dir.join("plugins/rome/TOOL.md")).unwrap();
        assert!(tool.starts_with("# rome"));
        assert!(tool.contains("`args`") && tool.contains("`agent_budget`"));
    }

    #[test]
    fn generate_pack_refuses_a_run_without_an_owner() {
        let root = tempfile::tempdir().unwrap();
        let error = generate_pack(&root.path().join("rome"), "rome", &run(vec![])).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<PromoteRefusal>(),
            Some(PromoteRefusal::NoTargetOwner)
        ));
    }

    #[test]
    fn effective_targets_falls_back_to_the_ceiling_only_when_the_union_is_empty() {
        let s = |items: &[&str]| {
            items
                .iter()
                .map(|&item| item.to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            effective_targets(&s(&["b", "a"]), &s(&["a"]), &s(&["c"])),
            s(&["a", "b"])
        );
        assert_eq!(effective_targets(&[], &[], &s(&["c"])), s(&["c"]));
    }

    #[test]
    fn generate_pack_uses_the_pin_ceiling_targets_when_the_run_spawned_nothing() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("rome");
        let mut run = run(vec!["gents/dynamic_workflows".into()]);
        run.script = "module.exports = () => ({result: 1})".into();
        run.view.pin.script_id = sha(&run.script);
        run.targets = effective_targets(&[], &[], &run.ceiling.targets);
        let pack = generate_pack(&dir, "rome", &run).unwrap();
        assert_eq!(pack.fixture["expect"], json!({"result": 1}));
        let config = read_json(&dir.join("pack_config.json"));
        let job = &config["tools"][0]["integrations"]["plugins"][0]["job"];
        assert_eq!(job["agent_targets"], json!(["wf-read-only"]));
        // `check_dir` stops at an unbuilt pack; this is the validation it runs once built.
        let settings = crate::tools::background::job::settings::parse(job.clone()).unwrap();
        assert_eq!(
            crate::tools::background::job::settings::validate(&settings),
            Vec::<String>::new()
        );
    }

    #[test]
    fn target_owners_are_the_packs_that_ship_the_targets() {
        let s = |items: &[&str]| {
            items
                .iter()
                .map(|&item| item.to_owned())
                .collect::<Vec<_>>()
        };
        // `acme/trusted` is what a caller block would trust; it ships no target.
        let shipped = vec![
            ("acme/trusted".to_owned(), vec![]),
            ("acme/targets".to_owned(), s(&["t1", "t2"])),
            ("acme/other".to_owned(), s(&["t3"])),
        ];
        assert_eq!(
            target_owners(&s(&["t2", "t1"]), &shipped),
            Some(s(&["acme/targets"]))
        );
        assert_eq!(
            target_owners(&s(&["t1", "t3"]), &shipped),
            Some(s(&["acme/other", "acme/targets"]))
        );
        assert_eq!(target_owners(&s(&["t1", "local"]), &shipped), None);
    }

    #[test]
    fn validate_name_rules() {
        let home = tempfile::tempdir().unwrap();
        for bad in ["Rome", "1x", "a-b", ""] {
            assert!(
                matches!(
                    validate_name(home.path(), bad),
                    Err(PromoteRefusal::BadName(_))
                ),
                "{bad}"
            );
        }
        validate_name(home.path(), "rome").unwrap();
        std::fs::create_dir_all(home.path().join("packs/promoted/rome")).unwrap();
        validate_name(home.path(), "rome").unwrap();
        let installed = home.path().join("plugins/installed/gents");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("rome.json"), "{}").unwrap();
        assert!(matches!(
            validate_name(home.path(), "rome"),
            Err(PromoteRefusal::NameTaken(_))
        ));
    }

    #[test]
    fn promotion_records_round_trip_and_list_newest_first() {
        let home = tempfile::tempdir().unwrap();
        assert!(list_promotions(home.path()).unwrap().is_empty());
        let record = |name: &str, at: &str| PromotionRecord {
            name: name.into(),
            plugin: format!("gents/{name}"),
            script_id: "abc".into(),
            address: "session-1:call_1".into(),
            session_id: "session-1".into(),
            promoted_at: at.into(),
        };
        let older = record("older", "2026-10-01T00:00:00Z");
        let newer = record("newer", "2026-10-02T00:00:00Z");
        write_promotion(home.path(), &older).unwrap();
        write_promotion(home.path(), &newer).unwrap();
        install(home.path(), "older");
        install(home.path(), "newer");
        assert!(home
            .path()
            .join("packs/promoted/newer/promotion.json")
            .exists());
        assert_eq!(list_promotions(home.path()).unwrap(), vec![newer, older]);
    }

    fn install(home: &Path, name: &str) {
        crate::pack::home_install::write_home_install(
            home,
            &crate::pack::home_install::HomePackInstall {
                coordinate: format!("{NAMESPACE}/{name}"),
                version: "0.1.0".into(),
                digest: format!("sha256:{}", "a".repeat(64)),
                kind: crate::pack::PackKind::Documents,
                assets: String::new(),
                dependency_assets: vec![],
                plugins: vec![],
                installed_at: "2026-10-01T00:00:00Z".into(),
                ux: vec![],
                installed_via: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn a_promotion_whose_pack_was_removed_is_not_listed() {
        let home = tempfile::tempdir().unwrap();
        let record = PromotionRecord {
            name: "rome".into(),
            plugin: "gents/rome".into(),
            script_id: "abc".into(),
            address: "session-1:call_1".into(),
            session_id: "session-1".into(),
            promoted_at: "2026-10-01T00:00:00Z".into(),
        };
        write_promotion(home.path(), &record).unwrap();
        assert!(list_promotions(home.path()).unwrap().is_empty());
        install(home.path(), "rome");
        assert_eq!(list_promotions(home.path()).unwrap(), vec![record]);
    }
}
