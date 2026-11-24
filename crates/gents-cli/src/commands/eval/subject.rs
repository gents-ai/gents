//! The eval subject is made by the functions init writes the Engineer's
//! documents with, on init's own default arguments, never from a copy.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cli::{CommandExecutionArg, EvalSubjectArgs};
use crate::commands::init::{
    default_agent_documents, default_engineer_init, wide_open_tools_document,
    DefaultAgentDocuments, DefaultAgentIds,
};

/// Suites' cells, checks and fixtures name the Engineer by these ids.
pub(crate) const SLOT: &str = "engineer";
const TOOLS_ID: &str = "engineer-tools";
const CONTEXT_ID: &str = "engineer-context";
const PROMPT_PATH: &str = "agents/engineer/system_prompt.md";
const NODE_DID: &str = "${GENTS_PACK_NODE_DID}";
const CONFIG: &str = "pack_config.json";
const MANIFEST: &str = "manifest.json";
const README: &str = "README.md";

const DOCUMENT_IDS: [(&str, &str); 6] = [
    ("agents", "agent_slot"),
    ("contexts", "context_id"),
    ("tools", "tools_id"),
    ("skills", "skill_id"),
    ("datastore_tool_surfaces", "surface_id"),
    ("agent_targets", "target_id"),
];

pub(crate) type SubjectFiles = BTreeMap<String, Vec<u8>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubjectFixture {
    pub(crate) name: String,
    pub(crate) why: String,
    #[serde(default)]
    pub(crate) documents: BTreeMap<String, Vec<Value>>,
    #[serde(default)]
    pub(crate) engineer: BTreeMap<String, Value>,
    #[serde(default)]
    pub(crate) slot_agents: Vec<String>,
    #[serde(default)]
    pub(crate) assets: Vec<String>,
}

#[derive(Deserialize)]
struct SuiteFile {
    #[serde(default)]
    subject_fixtures: Vec<SubjectFixture>,
}

pub(super) fn write(args: &EvalSubjectArgs, out: &mut dyn Write) -> Result<()> {
    anyhow::ensure!(
        !args.out.exists(),
        "{} already exists; the subject is written into a new directory",
        args.out.display()
    );
    let mut files = match &args.from {
        Some(dir) => read_pack_dir(dir)?,
        None => engineer_subject(CommandExecutionArg::Unrestricted)?,
    };
    if args.command_execution == CommandExecutionArg::Off {
        turn_off_commands(&mut files)?;
    }
    let fixtures = match &args.suite {
        Some(dir) => suite_fixtures(dir)?,
        None => Vec::new(),
    };
    for fixture in &fixtures {
        let suite = args
            .suite
            .as_deref()
            .context("fixtures come from a suite")?;
        apply_fixture(&mut files, suite, fixture)?;
    }
    for (path, bytes) in &files {
        let destination = args.out.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&destination, bytes)
            .with_context(|| format!("writing {}", destination.display()))?;
    }
    super::write_json(
        out,
        &json!({
            "subject": args.out,
            "from": args.from.as_ref().map_or_else(|| json!("gents init --engineer"), |dir| json!(dir)),
            "agent_slot": SLOT,
            "command_execution": args.command_execution,
            "fixtures": fixtures.iter().map(|fixture| &fixture.name).collect::<Vec<_>>(),
            "files": files.keys().collect::<Vec<_>>(),
        }),
    )
}

/// Command execution `off` changes only the Engineer's host bash mode.
pub(crate) fn engineer_subject(command_execution: CommandExecutionArg) -> Result<SubjectFiles> {
    let (args, tool_package) = default_engineer_init()?;
    let agent_did = gents_pack::pack::agent_slot_reference(SLOT);
    let DefaultAgentDocuments {
        tools,
        context,
        display_name,
        description,
        tags,
        engineer,
    } = default_agent_documents(
        &args,
        tool_package,
        &DefaultAgentIds {
            node_did: NODE_DID,
            agent_did: &agent_did,
            tools_id: TOOLS_ID,
            context_id: CONTEXT_ID,
        },
        None,
    );
    let (surface, own_target) = engineer.context("`gents init --engineer` made no Engineer")?;
    let prompt = context
        .system_prompt
        .clone()
        .context("the Engineer's Context has no system prompt")?;
    let mut context = document(&context)?;
    context["system_prompt"] = json!(format!("./{PROMPT_PATH}"));
    let config = json!({
        "node": {},
        "agents": [{
            "agent_slot": SLOT,
            "display_name": display_name,
            "description": description,
            "context_id": CONTEXT_ID,
            "inference_profile_id": gents_pack::pack::inference_slot_reference(SLOT),
            "enabled": true,
            "tags": tags,
        }],
        "contexts": [context],
        "tools": [document(&tools)?, document(&wide_open_tools_document(NODE_DID))?],
        "datastore_tool_surfaces": [document(&surface)?],
        "agent_targets": [document(&own_target)?],
    });
    let manifest = json!({
        "manifest_version": 1,
        "name": "engineer",
        "version": "0.1.0",
        "description": "The Engineer `gents init --engineer` makes on a fresh home, as an eval subject. A trial installs the definition's core packs on it as init does.",
        "authors": ["gents-ai contributors"],
        "tags": ["eval"],
        "kind": "documents",
        "assets": [README, PROMPT_PATH, CONFIG],
        "config": CONFIG,
        "inference_slots": [{
            "name": SLOT,
            "description": "Runs the Engineer and every agent its core packs ship.",
            "agents": [SLOT],
        }],
    });
    let mut files = SubjectFiles::from([
        (MANIFEST.to_owned(), pretty(&manifest)?),
        (
            README.to_owned(),
            b"# engineer\n\nThe Engineer `gents init --engineer` makes, written by `gents utils eval subject`.\n"
                .to_vec(),
        ),
        (CONFIG.to_owned(), pretty(&config)?),
        (PROMPT_PATH.to_owned(), prompt.into_bytes()),
    ]);
    if command_execution == CommandExecutionArg::Off {
        turn_off_commands(&mut files)?;
    }
    Ok(files)
}

/// An embedded trial shares this host, so its Engineer has no bash and,
/// with it, no background processes.
fn turn_off_commands(files: &mut SubjectFiles) -> Result<()> {
    let mut config = read_json(files, CONFIG)?;
    let tools = engineer_document(&mut config, "tools")?;
    let host = object(tools)?.entry("host").or_insert_with(|| json!({}));
    let bash = object(host)?.entry("bash").or_insert_with(|| json!({}));
    object(bash)?.insert("mode".to_owned(), json!("Off"));
    files.insert(CONFIG.to_owned(), pretty(&config)?);
    Ok(())
}

pub(crate) fn suite_fixtures(dir: &Path) -> Result<Vec<SubjectFixture>> {
    let path = dir.join("suite.json");
    let suite: SuiteFile = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing the subject fixtures of {}", path.display()))?;
    Ok(suite.subject_fixtures)
}

/// A fixture only adds: one that would change or drop anything the subject
/// has is refused and leaves `files` as they were.
pub(crate) fn apply_fixture(
    files: &mut SubjectFiles,
    suite: &Path,
    fixture: &SubjectFixture,
) -> Result<()> {
    let name = &fixture.name;
    let mut layered = files.clone();
    let mut config = read_json(&layered, CONFIG)?;
    let mut manifest = read_json(&layered, MANIFEST)?;
    for (key, rows) in &fixture.documents {
        let id = DOCUMENT_IDS
            .iter()
            .find(|(document, _)| document == key)
            .map(|(_, id)| *id)
            .with_context(|| {
                format!(
                    "fixture {name}: {key} is not one of {:?}",
                    DOCUMENT_IDS.map(|(document, _)| document)
                )
            })?;
        let present = object(&mut config)?
            .entry(key.clone())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("the subject's {key} is not a list"))?;
        for row in rows {
            anyhow::ensure!(
                !present.iter().any(|existing| existing[id] == row[id]),
                "fixture {name}: the subject already has {key} {}; a fixture only adds",
                row[id]
            );
            present.push(row.clone());
        }
    }
    for (part, addition) in &fixture.engineer {
        anyhow::ensure!(
            matches!(part.as_str(), "context" | "tools"),
            "fixture {name}: the Engineer's {part} is not its context or tools"
        );
        add(
            engineer_document(&mut config, part)?,
            addition,
            &format!("fixture {name}: engineer {part}"),
        )?;
    }
    if !fixture.slot_agents.is_empty() {
        let mut slots: Vec<&mut Value> = manifest["inference_slots"]
            .as_array_mut()
            .context("the subject declares no inference slots")?
            .iter_mut()
            .filter(|slot| {
                slot["agents"]
                    .as_array()
                    .is_some_and(|agents| agents.contains(&json!(SLOT)))
            })
            .collect();
        anyhow::ensure!(
            slots.len() == 1,
            "fixture {name}: no one inference slot runs the {SLOT}"
        );
        let slot = slots.remove(0);
        add(
            slot,
            &json!({"agents": fixture.slot_agents}),
            &format!("fixture {name}: inference slot"),
        )?;
    }
    for asset in &fixture.assets {
        anyhow::ensure!(
            Path::new(asset)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "fixture {name}: asset {asset} is not a path inside its suite"
        );
        for (relative, bytes) in asset_files(suite, asset)? {
            let declared = manifest["assets"]
                .as_array_mut()
                .context("the subject declares no assets")?;
            anyhow::ensure!(
                !layered.contains_key(&relative) && !declared.contains(&json!(relative)),
                "fixture {name}: the subject already has {relative}; a fixture only adds"
            );
            declared.push(json!(relative));
            layered.insert(relative, bytes);
        }
    }
    layered.insert(CONFIG.to_owned(), pretty(&config)?);
    layered.insert(MANIFEST.to_owned(), pretty(&manifest)?);
    *files = layered;
    Ok(())
}

/// Sets fields `base` leaves unset and appends list items; refuses any
/// other difference.
fn add(base: &mut Value, addition: &Value, at: &str) -> Result<()> {
    let (Some(base), Some(addition)) = (base.as_object_mut(), addition.as_object()) else {
        anyhow::bail!("{at}: a fixture adds fields to an object");
    };
    for (key, value) in addition {
        match base.get_mut(key) {
            None | Some(Value::Null) => {
                base.insert(key.clone(), value.clone());
            }
            Some(current @ Value::Object(_)) if value.is_object() => {
                add(current, value, &format!("{at}.{key}"))?;
            }
            Some(Value::Array(current)) if value.is_array() => {
                for item in value.as_array().into_iter().flatten() {
                    if !current.contains(item) {
                        current.push(item.clone());
                    }
                }
            }
            Some(current) => anyhow::ensure!(
                current == value,
                "{at}.{key}: a fixture may not change {current} to {value}; it only adds"
            ),
        }
    }
    Ok(())
}

fn engineer_document<'a>(config: &'a mut Value, part: &str) -> Result<&'a mut Value> {
    let context_id = config["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|agent| agent["agent_slot"] == SLOT)
        .and_then(|agent| agent["context_id"].as_str())
        .with_context(|| format!("the subject has no {SLOT} agent with a Context"))?
        .to_owned();
    let tools_id = config["contexts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|context| context["context_id"] == context_id.as_str())
        .with_context(|| format!("the {SLOT}'s Context {context_id} is not in the subject"))?
        ["tools_id"]
        .as_str()
        .map(str::to_owned);
    let (key, field, id) = match part {
        "context" => ("contexts", "context_id", Some(context_id)),
        _ => ("tools", "tools_id", tools_id),
    };
    let id = id.with_context(|| format!("the {SLOT}'s Context selects no Tools"))?;
    config[key]
        .as_array_mut()
        .into_iter()
        .flatten()
        .find(|document| document[field] == id.as_str())
        .with_context(|| format!("the {SLOT}'s {part} {id} is not in the subject"))
}

fn asset_files(suite: &Path, asset: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let root = suite.join(asset);
    let mut pending = vec![root.clone()];
    let mut found = Vec::new();
    while let Some(path) = pending.pop() {
        let kind = std::fs::symlink_metadata(&path)
            .with_context(|| format!("reading fixture asset {}", path.display()))?
            .file_type();
        anyhow::ensure!(
            !kind.is_symlink(),
            "fixture asset {} is a symlink",
            path.display()
        );
        if kind.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
            continue;
        }
        let relative = path
            .strip_prefix(suite)?
            .to_str()
            .with_context(|| format!("{} is not UTF-8", path.display()))?
            .replace('\\', "/");
        found.push((relative, std::fs::read(&path)?));
    }
    found.sort();
    Ok(found)
}

fn read_pack_dir(dir: &Path) -> Result<SubjectFiles> {
    let mut files = SubjectFiles::new();
    for (relative, bytes) in asset_files(dir, ".")? {
        files.insert(relative.trim_start_matches("./").to_owned(), bytes);
    }
    anyhow::ensure!(
        files.contains_key(MANIFEST) && files.contains_key(CONFIG),
        "{} is not a subject pack: it needs {MANIFEST} and {CONFIG}",
        dir.display()
    );
    Ok(files)
}

fn read_json(files: &SubjectFiles, path: &str) -> Result<Value> {
    serde_json::from_slice(
        files
            .get(path)
            .with_context(|| format!("the subject has no {path}"))?,
    )
    .with_context(|| format!("parsing the subject's {path}"))
}

fn object(value: &mut Value) -> Result<&mut serde_json::Map<String, Value>> {
    value.as_object_mut().context("expected a JSON object")
}

/// The install binds a pack document's `node_did`.
fn document<T>(value: &T) -> Result<Value>
where
    T: Serialize,
{
    let mut value = serde_json::to_value(value)?;
    drop_nulls(&mut value);
    object(&mut value)?.remove("node_did");
    Ok(value)
}

fn drop_nulls(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|_, field| !field.is_null());
            fields.values_mut().for_each(drop_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(drop_nulls),
        _ => {}
    }
}

fn pretty(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests;
