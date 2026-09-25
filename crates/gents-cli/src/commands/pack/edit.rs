//! `gents pack add`, `gents pack remove-part` and `gents pack fmt`: editing a
//! pack directory while keeping `manifest.json` consistent with its files.
//!
//! Every edit writes the file it needs and records it in the manifest
//! (assets, schemas, inference slots, plugins) in the same step, so the asset
//! list is never maintained by hand. `manifest.json` and `pack_config.json`
//! are written in canonical form (sorted keys, two-space indent, trailing
//! newline), which is also what `fmt` writes, so an edit's diff is the edit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::cli::{PackAddArgs, PackAddCommand, PackFmtArgs, PackPart, PackRemovePartArgs};

const CONFIG: &str = "pack_config.json";

pub(super) struct PackEdit {
    pub(super) dir: PathBuf,
    pub(super) manifest: Value,
    config: Option<Value>,
}

impl PackEdit {
    pub(super) fn load(dir: &Path) -> Result<Self> {
        let manifest = read_json(&dir.join("manifest.json"))?;
        let config = match manifest.get("config").and_then(Value::as_str) {
            Some(path) => Some(read_json(&dir.join(path))?),
            None => None,
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
            config,
        })
    }

    pub(super) fn save(&self) -> Result<()> {
        write_json(&self.dir.join("manifest.json"), &self.manifest)?;
        if let (Some(config), Some(path)) = (
            &self.config,
            self.manifest.get("config").and_then(Value::as_str),
        ) {
            write_json(&self.dir.join(path), config)?;
        }
        Ok(())
    }

    fn kind(&self) -> &str {
        self.manifest["kind"].as_str().unwrap_or_default()
    }

    /// The configuration, created (and the pack made a documents pack) when
    /// an assets pack gains its first document.
    pub(super) fn config(&mut self) -> Result<&mut Map<String, Value>> {
        if self.config.is_none() {
            anyhow::ensure!(
                matches!(self.kind(), "assets" | "documents" | "graph"),
                "a {} pack carries no configuration",
                self.kind()
            );
            self.manifest["kind"] = json!(if self.kind() == "graph" {
                "graph"
            } else {
                "documents"
            });
            self.manifest["config"] = json!(CONFIG);
            self.add_asset(CONFIG);
            self.config = Some(json!({ "agent_principal": {} }));
        }
        self.config
            .as_mut()
            .and_then(Value::as_object_mut)
            .context("pack_config.json is not a JSON object")
    }

    pub(super) fn list(&mut self, key: &str) -> Result<&mut Vec<Value>> {
        list_in(self.config()?, key)
    }

    pub(super) fn manifest_list(&mut self, key: &str) -> Result<&mut Vec<Value>> {
        list_in(
            self.manifest
                .as_object_mut()
                .context("manifest.json is not a JSON object")?,
            key,
        )
    }

    fn has(&self, key: &str, field: &str, id: &str) -> bool {
        self.config
            .as_ref()
            .and_then(|config| config.get(key))
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| row[field] == id))
    }

    fn require_new(&self, key: &str, field: &str, id: &str) -> Result<()> {
        anyhow::ensure!(!self.has(key, field, id), "{key} already has {id:?}");
        Ok(())
    }

    fn add_asset(&mut self, path: &str) {
        if let Ok(assets) = self.manifest_list("assets") {
            if !assets.iter().any(|asset| asset == path) {
                assets.push(json!(path));
                assets.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
        }
    }

    fn remove_asset(&mut self, path: &str) -> Result<()> {
        self.manifest_list("assets")?.retain(|asset| asset != path);
        if let Ok(schemas) = self.manifest_list("schemas") {
            schemas.retain(|schema| schema != path);
        }
        let file = self.dir.join(path);
        if file.is_file() {
            std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?;
        }
        Ok(())
    }

    /// Writes a new file and declares it.
    pub(super) fn create_file(&mut self, path: &str, contents: impl AsRef<[u8]>) -> Result<()> {
        let target = self.dir.join(path);
        anyhow::ensure!(!target.exists(), "{path} already exists");
        std::fs::create_dir_all(target.parent().context("file has no parent")?)?;
        std::fs::write(&target, contents).with_context(|| format!("writing {path}"))?;
        self.add_asset(path);
        Ok(())
    }

    fn remove_row(&mut self, key: &str, field: &str, id: &str) -> Result<Value> {
        let rows = self.list(key)?;
        let index = rows
            .iter()
            .position(|row| row[field] == id)
            .with_context(|| format!("{key} has no {id:?}"))?;
        Ok(rows.remove(index))
    }
}

fn list_in<'a>(object: &'a mut Map<String, Value>, key: &str) -> Result<&'a mut Vec<Value>> {
    object
        .entry(key)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .with_context(|| format!("{key} is not a list"))
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("writing {}", path.display()))
}

/// `review-bot` -> `review_bot`: document ids are kebab-case, directories
/// snake_case.
fn snake(id: &str) -> String {
    id.replace('-', "_")
}

fn title(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn check_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && id.as_bytes()[0].is_ascii_lowercase(),
        "{id:?} is not a valid id; use lowercase letters, digits and dashes"
    );
    Ok(())
}

pub(crate) fn fmt(args: PackFmtArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    PackEdit::load(&dir)?.save()
}

pub(crate) fn add(args: PackAddArgs) -> Result<()> {
    let dir = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut pack = PackEdit::load(&dir)?;
    apply_add(&mut pack, args.command)?;
    pack.save()
}

pub(super) fn apply_add(pack: &mut PackEdit, command: PackAddCommand) -> Result<()> {
    match command {
        PackAddCommand::Behavior { id, slot } => add_behavior(pack, &id, slot.as_deref()),
        PackAddCommand::Task { id, behavior } => {
            check_id(&id)?;
            pack.require_new("tasks", "task_id", &id)?;
            anyhow::ensure!(
                pack.has("agent_behaviors", "behavior_id", &behavior),
                "the pack has no behavior {behavior:?}; add it first"
            );
            let prompt = format!("tasks/{}/prompt.md", snake(&id));
            pack.create_file(&prompt, "Describe what this task does.\n")?;
            pack.list("tasks")?.push(json!({
                "task_id": id,
                "display_name": title(&id),
                "behavior_id": behavior,
                "prompt_template": format!("./{prompt}"),
            }));
            Ok(())
        }
        PackAddCommand::Trigger { id, task, on } => {
            check_id(&id)?;
            pack.require_new("triggers", "trigger_id", &id)?;
            pack.require_new("event_sources", "event_source_id", &id)?;
            anyhow::ensure!(
                pack.has("tasks", "task_id", &task),
                "the pack has no task {task:?}; add it first"
            );
            pack.list("event_sources")?.push(json!({
                "event_source_id": id,
                "display_name": format!("{on} created"),
                "source_collection": on,
                "event_kind": "created",
            }));
            pack.list("triggers")?.push(json!({
                "trigger_id": id,
                "display_name": title(&id),
                "task_id": task,
                "source": {"kind": "event", "event_source_id": id},
                "concurrency": "serial",
            }));
            Ok(())
        }
        PackAddCommand::Schema { collection } => {
            anyhow::ensure!(
                collection
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                    && collection.chars().all(|c| c.is_ascii_alphanumeric()),
                "{collection:?} is not a collection name; use PascalCase"
            );
            let path = format!("schemas/{}.graphql", super_snake(&collection));
            pack.create_file(
                &path,
                &format!("type {collection} {{\n  run_id: String @index\n}}\n"),
            )?;
            pack.manifest_list("schemas")?.push(json!(path));
            Ok(())
        }
        PackAddCommand::Skill { id, from } => {
            check_id(&id)?;
            pack.require_new("skills", "skill_id", &id)?;
            let body = match from {
                Some(path) => std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?,
                None => format!("# {}\n\nWhen and how to use this skill.\n", title(&id)),
            };
            let path = format!("skills/{}/SKILL.md", snake(&id));
            pack.create_file(&path, &body)?;
            pack.list("skills")?.push(json!({
                "skill_id": id,
                "name": id,
                "instructions": format!("./{path}"),
            }));
            Ok(())
        }
        PackAddCommand::Plugin {
            name,
            language,
            prebuilt,
        } => add_plugin(pack, &name, language.as_deref(), prebuilt.as_deref()),
        PackAddCommand::Graph { graph_id } => {
            check_id(&graph_id)?;
            pack.require_new("graph_intents", "graph_id", &graph_id)?;
            pack.config()?;
            pack.manifest["kind"] = json!("graph");
            pack.manifest["compiler_version"] = json!(gents::graph_pipeline::COMPILER_VERSION);
            pack.list("graphs")?.push(json!({ "graph_id": graph_id }));
            pack.list("graph_intents")?.push(json!({
                "graph_id": graph_id,
                "nodes": [],
                "entries": [],
                "results": [],
                "limits": {
                    "max_nodes": 8, "max_edges": 16, "max_depth": 8, "max_fan_out": 16,
                    "max_total_invocations": 128, "max_runtime_secs": 3600,
                },
            }));
            Ok(())
        }
        PackAddCommand::Stage {
            node,
            graph,
            task,
            plugin,
            input,
            output,
            from,
        } => {
            let target = match (task, plugin) {
                (Some(task), _) => {
                    anyhow::ensure!(
                        pack.has("tasks", "task_id", &task),
                        "the pack has no task {task:?}; add it first"
                    );
                    json!({"kind": "task", "task_id": task})
                }
                (None, Some(plugin)) => {
                    anyhow::ensure!(
                        pack.manifest["plugins"]
                            .as_array()
                            .is_some_and(|rows| rows.iter().any(|row| row["name"] == plugin)),
                        "the pack has no plugin {plugin:?}; add it first"
                    );
                    json!({"kind": "plugin", "plugin": plugin})
                }
                (None, None) => anyhow::bail!("name the stage's --task or --plugin"),
            };
            add_stage(pack, &node, &graph, target, &input, &output, from.as_deref())
        }
    }
}

/// `ReviewJob` -> `review_job`, for a schema file name.
fn super_snake(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() && index > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

fn add_behavior(pack: &mut PackEdit, id: &str, slot: Option<&str>) -> Result<()> {
    check_id(id)?;
    pack.require_new("agent_behaviors", "behavior_id", id)?;
    let slot = slot.map(str::to_owned).unwrap_or_else(|| snake(id));
    let prompt = format!("agent_behaviors/{}/system_prompt.md", snake(id));
    pack.create_file(&prompt, &format!("You are the {} agent.\n", title(id)))?;
    pack.list("agent_behaviors")?.push(json!({
        "behavior_id": id,
        "display_name": title(id),
        "context_id": format!("{id}-context"),
        "inference_profile_id": format!("gents:inference-slot:{slot}"),
    }));
    pack.list("contexts")?.push(json!({
        "context_id": format!("{id}-context"),
        "display_name": title(id),
        "system_prompt": format!("./{prompt}"),
        "tools_id": format!("{id}-tools"),
    }));
    pack.list("tools")?.push(json!({
        "tools_id": format!("{id}-tools"),
        "display_name": format!("{} tools", title(id)),
    }));
    let slots = pack.manifest_list("inference_slots")?;
    match slots.iter_mut().find(|entry| entry["name"] == slot) {
        Some(entry) => list_in(
            entry
                .as_object_mut()
                .context("an inference slot is not an object")?,
            "behaviors",
        )?
        .push(json!(id)),
        None => slots.push(json!({
            "name": slot,
            "description": format!("The profile the {} behavior runs on.", title(id)),
            "behaviors": [id],
        })),
    }
    Ok(())
}

fn add_plugin(
    pack: &mut PackEdit,
    name: &str,
    language: Option<&str>,
    prebuilt: Option<&Path>,
) -> Result<()> {
    anyhow::ensure!(
        name.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase),
        "plugin names are snake_case"
    );
    anyhow::ensure!(
        !pack.manifest["plugins"]
            .as_array()
            .is_some_and(|plugins| plugins.iter().any(|plugin| plugin["name"] == name)),
        "the pack already has a plugin {name:?}"
    );
    let artifact = format!("plugins/{name}.afb");
    let mut entry = json!({
        "name": name,
        "description": format!("{} tool.", title(name)),
        "artifact": artifact,
        "input_schema": {"type": "object"},
    });
    match prebuilt {
        Some(file) => {
            let bytes =
                std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
            let afb = afterburner_cloud::Afb::from_bytes(&bytes)
                .with_context(|| format!("{} is not a compiled plugin", file.display()))?;
            entry["language"] = json!(afb.manifest.package.language);
            let target = pack.dir.join(&artifact);
            anyhow::ensure!(!target.exists(), "{artifact} already exists");
            std::fs::create_dir_all(target.parent().context("artifact parent")?)?;
            std::fs::write(&target, bytes).with_context(|| format!("writing {artifact}"))?;
        }
        None => {
            let names_language = super::scaffold::write_plugin_source(
                &pack.dir.join("plugins").join(name),
                name,
                language,
            )?;
            entry["language"] = json!(names_language);
            entry["source"] = json!(format!("plugins/{name}"));
        }
    }
    pack.add_asset(&artifact);
    pack.manifest_list("plugins")?.push(entry);
    Ok(())
}

fn add_stage(
    pack: &mut PackEdit,
    node: &str,
    graph: &str,
    target: Value,
    input: &str,
    output: &str,
    from: Option<&str>,
) -> Result<()> {
    check_id(node)?;
    let capability = format!("{graph}-{node}");
    pack.require_new("graph_capabilities", "capability_id", &capability)?;
    pack.list("graph_capabilities")?.push(json!({
        "capability_id": capability,
        "allowed_callers": ["${GENTS_PACK_AGENT_DID}"],
        "revision": "v1",
        "target": target,
        "input_ports": [{
            "name": "input", "collection": input, "schema": format!("{input}/v1"),
            "correlation_field": "run_id", "cardinality": "one", "required": true,
        }],
        "output_ports": [{
            "name": "output", "collection": output, "schema": format!("{output}/v1"),
            "correlation_field": "run_id", "cardinality": "one",
        }],
    }));
    let intents = pack.list("graph_intents")?;
    let intent = intents
        .iter_mut()
        .find(|intent| intent["graph_id"] == graph)
        .with_context(|| format!("the pack has no graph {graph:?}; add it first"))?;
    let intent = intent
        .as_object_mut()
        .context("a graph intent is not an object")?;
    anyhow::ensure!(
        !list_in(intent, "nodes")?
            .iter()
            .any(|existing| existing["node_id"] == node),
        "graph {graph:?} already has a stage {node:?}"
    );
    list_in(intent, "nodes")?.push(json!({
        "node_id": node, "capability_id": capability, "capability_revision": "v1",
    }));
    match from {
        None => {
            anyhow::ensure!(
                list_in(intent, "entries")?.is_empty(),
                "graph {graph:?} already has an entry; name the upstream stage with --from node.port"
            );
            list_in(intent, "entries")?.push(json!({
                "name": "start", "collection": input, "schema": format!("{input}/v1"),
                "input_contract": format!("{graph}-input/v1"),
                "to": {"node_id": node, "port": "input"},
            }));
        }
        Some(from) => {
            let (from_node, from_port) = from
                .split_once('.')
                .context("--from is node.port, for example scan.output")?;
            list_in(intent, "edges")?.push(json!({
                "from": {"node_id": from_node, "port": from_port},
                "to": {"node_id": node, "port": "input"},
                "concurrency": "serial",
            }));
            list_in(intent, "results")?.retain(|result| result["from"]["node_id"] != from_node);
        }
    }
    let results = list_in(intent, "results")?;
    if results.is_empty() {
        results.push(json!({
            "name": "result",
            "from": {"node_id": node, "port": "output"},
            "cardinality": {"kind": "exactly", "count": 1},
            "terminal": true,
        }));
    }
    Ok(())
}

pub(crate) fn remove_part(args: PackRemovePartArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    let mut pack = PackEdit::load(&dir)?;
    let id = args.id.as_str();
    match args.part {
        PackPart::Behavior => {
            let behavior = pack.remove_row("agent_behaviors", "behavior_id", id)?;
            anyhow::ensure!(
                !pack.config.as_ref().is_some_and(|config| config["tasks"]
                    .as_array()
                    .is_some_and(|tasks| tasks.iter().any(|task| task["behavior_id"] == id))),
                "a task still runs behavior {id:?}; remove it first"
            );
            if let Some(context_id) = behavior["context_id"].as_str() {
                let context = pack.remove_row("contexts", "context_id", context_id)?;
                if let Some(tools_id) = context["tools_id"].as_str() {
                    let _ = pack.remove_row("tools", "tools_id", tools_id);
                }
                if let Some(prompt) = context["system_prompt"].as_str() {
                    pack.remove_asset(prompt.trim_start_matches("./"))?;
                }
            }
            for slot in pack.manifest_list("inference_slots")? {
                if let Some(behaviors) = slot["behaviors"].as_array_mut() {
                    behaviors.retain(|behavior| behavior != id);
                }
            }
            pack.manifest_list("inference_slots")?
                .retain(|slot| slot["behaviors"].as_array().is_some_and(|b| !b.is_empty()));
        }
        PackPart::Task => {
            let task = pack.remove_row("tasks", "task_id", id)?;
            if let Some(prompt) = task["prompt_template"].as_str() {
                pack.remove_asset(prompt.trim_start_matches("./"))?;
            }
        }
        PackPart::Trigger => {
            let trigger = pack.remove_row("triggers", "trigger_id", id)?;
            if let Some(source) = trigger["source"]["event_source_id"].as_str() {
                let _ = pack.remove_row("event_sources", "event_source_id", source);
            }
        }
        PackPart::Skill => {
            let skill = pack.remove_row("skills", "skill_id", id)?;
            if let Some(path) = skill["instructions"].as_str() {
                if path.starts_with("./") {
                    pack.remove_asset(path.trim_start_matches("./"))?;
                }
            }
        }
        PackPart::Schema => {
            let path = format!("schemas/{}.graphql", super_snake(id));
            anyhow::ensure!(
                pack.manifest["assets"]
                    .as_array()
                    .is_some_and(|assets| assets.iter().any(|asset| asset == &json!(path))),
                "the pack has no schema {id:?}"
            );
            pack.remove_asset(&path)?;
        }
        PackPart::Plugin => {
            let plugins = pack.manifest_list("plugins")?;
            let index = plugins
                .iter()
                .position(|plugin| plugin["name"] == id)
                .with_context(|| format!("the pack has no plugin {id:?}"))?;
            let plugin = plugins.remove(index);
            if let Some(artifact) = plugin["artifact"].as_str() {
                pack.remove_asset(artifact)?;
            }
            if let Some(source) = plugin["source"].as_str() {
                let source = pack.dir.join(source);
                if source.is_dir() {
                    std::fs::remove_dir_all(&source)
                        .with_context(|| format!("removing {}", source.display()))?;
                }
            }
        }
        PackPart::Graph => {
            pack.remove_row("graph_intents", "graph_id", id)?;
            let _ = pack.remove_row("graphs", "graph_id", id);
            let prefix = format!("{id}-");
            pack.list("graph_capabilities")?.retain(|capability| {
                !capability["capability_id"]
                    .as_str()
                    .is_some_and(|capability| capability.starts_with(&prefix))
            });
        }
    }
    pack.save()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PackTemplate;

    async fn check(dir: &Path) -> Vec<String> {
        super::super::check::check_dir(dir).await.problems
    }

    fn scaffolded(template: PackTemplate) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        super::super::scaffold::scaffold(
            &dir,
            "review_toolkit",
            &crate::cli::PackScaffoldArgs {
                kind: None,
                namespace: "acme".into(),
                template: Some(template),
                language: None,
            },
        )
        .unwrap();
        (root, dir)
    }

    fn add(dir: &Path, command: PackAddCommand) {
        let mut pack = PackEdit::load(dir).unwrap();
        apply_add(&mut pack, command).unwrap();
        pack.save().unwrap();
    }

    fn remove(dir: &Path, part: PackPart, id: &str) {
        remove_part(PackRemovePartArgs {
            part,
            id: id.into(),
            dir: Some(dir.to_path_buf()),
        })
        .unwrap();
    }

    #[tokio::test]
    async fn parts_added_to_an_assets_pack_keep_it_valid_and_removal_restores_it() {
        let (_root, dir) = scaffolded(PackTemplate::Assets);
        add(
            &dir,
            PackAddCommand::Behavior {
                id: "reviewer".into(),
                slot: None,
            },
        );
        add(
            &dir,
            PackAddCommand::Task {
                id: "review".into(),
                behavior: "reviewer".into(),
            },
        );
        add(
            &dir,
            PackAddCommand::Schema {
                collection: "ReviewJob".into(),
            },
        );
        add(
            &dir,
            PackAddCommand::Trigger {
                id: "on-job".into(),
                task: "review".into(),
                on: "ReviewJob".into(),
            },
        );
        add(
            &dir,
            PackAddCommand::Skill {
                id: "triage".into(),
                from: None,
            },
        );
        let problems = check(&dir).await;
        assert!(problems.is_empty(), "{problems:#?}");

        remove(&dir, PackPart::Trigger, "on-job");
        remove(&dir, PackPart::Skill, "triage");
        remove(&dir, PackPart::Task, "review");
        remove(&dir, PackPart::Behavior, "reviewer");
        remove(&dir, PackPart::Schema, "ReviewJob");
        let problems = check(&dir).await;
        assert!(problems.is_empty(), "{problems:#?}");
        let manifest = read_json(&dir.join("manifest.json")).unwrap();
        assert_eq!(
            manifest["assets"],
            json!(["README.md", "pack_config.json"]),
            "every file an add wrote is removed with its part"
        );
    }

    #[tokio::test]
    async fn a_graph_is_built_stage_by_stage_and_compiles() {
        let (_root, dir) = scaffolded(PackTemplate::Minimal);
        add(
            &dir,
            PackAddCommand::Schema {
                collection: "ReviewJob".into(),
            },
        );
        add(
            &dir,
            PackAddCommand::Schema {
                collection: "ReviewNotes".into(),
            },
        );
        add(
            &dir,
            PackAddCommand::Schema {
                collection: "ReviewReport".into(),
            },
        );
        add(
            &dir,
            PackAddCommand::Graph {
                graph_id: "review".into(),
            },
        );
        add(
            &dir,
            PackAddCommand::Stage {
                node: "scan".into(),
                graph: "review".into(),
                task: Some("review-toolkit-worker-task".into()),
                plugin: None,
                input: "ReviewJob".into(),
                output: "ReviewNotes".into(),
                from: None,
            },
        );
        add(
            &dir,
            PackAddCommand::Stage {
                node: "report".into(),
                graph: "review".into(),
                task: Some("review-toolkit-worker-task".into()),
                plugin: None,
                input: "ReviewNotes".into(),
                output: "ReviewReport".into(),
                from: Some("scan.output".into()),
            },
        );
        super::super::check::write_topology(&dir).unwrap();
        let report = super::super::check::check_dir(&dir).await;
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
        assert_eq!(report.graphs, vec!["review".to_owned()]);
    }

    #[test]
    fn duplicates_and_dangling_references_are_refused() {
        let (_root, dir) = scaffolded(PackTemplate::Minimal);
        let mut pack = PackEdit::load(&dir).unwrap();
        let duplicate = apply_add(
            &mut pack,
            PackAddCommand::Behavior {
                id: "review-toolkit-worker".into(),
                slot: None,
            },
        )
        .unwrap_err();
        assert!(
            format!("{duplicate:#}").contains("already has"),
            "{duplicate:#}"
        );
        let dangling = apply_add(
            &mut pack,
            PackAddCommand::Task {
                id: "orphan".into(),
                behavior: "nobody".into(),
            },
        )
        .unwrap_err();
        assert!(
            format!("{dangling:#}").contains("no behavior"),
            "{dangling:#}"
        );
    }

    #[test]
    fn fmt_is_idempotent_and_changes_no_meaning() {
        let (_root, dir) = scaffolded(PackTemplate::Automation);
        let before = read_json(&dir.join(CONFIG)).unwrap();
        fmt(PackFmtArgs {
            dir: Some(dir.clone()),
        })
        .unwrap();
        let once = std::fs::read(dir.join(CONFIG)).unwrap();
        fmt(PackFmtArgs {
            dir: Some(dir.clone()),
        })
        .unwrap();
        assert_eq!(std::fs::read(dir.join(CONFIG)).unwrap(), once);
        assert_eq!(read_json(&dir.join(CONFIG)).unwrap(), before);
    }
}
