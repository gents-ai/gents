use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use super::names::Names;
use super::scaffold::{tool_markdown, ux_plugin, write_plugin_source};
use crate::pack::rooted_paths::RootedPaths;

const CONFIG: &str = "pack_config.json";

/// An editable pack directory. Every file it reads or writes resolves through
/// the rooted path owner and must stay inside the pack, so a manifest naming
/// `../` or a symlink in the tree cannot reach beyond it.
pub struct PackEdit {
    context: RootedPaths,
    pub dir: PathBuf,
    pub manifest: Value,
    config: Option<Value>,
}

/// One part to add to a pack.
pub enum PackAddition {
    Agent {
        id: String,
        slot: Option<String>,
    },
    Task {
        id: String,
        agent: String,
    },
    Trigger {
        id: String,
        task: String,
        on: String,
    },
    Plugin {
        name: String,
        language: Option<String>,
        prebuilt: Option<PathBuf>,
    },
    Skill {
        id: String,
        from: Option<PathBuf>,
    },
    Schema {
        collection: String,
    },
    Page {
        name: String,
    },
}

/// The kinds of part `remove_part` deletes.
#[derive(Clone, Copy, Debug)]
pub enum PackPart {
    Agent,
    Task,
    Trigger,
    Plugin,
    Skill,
    Schema,
}

impl PackEdit {
    pub fn load(dir: &Path) -> Result<Self> {
        let context = RootedPaths::new(dir)
            .with_context(|| format!("opening the pack directory {}", dir.display()))?;
        let dir = std::fs::canonicalize(dir)
            .with_context(|| format!("opening the pack directory {}", dir.display()))?;
        let mut pack = Self {
            context,
            dir,
            manifest: Value::Null,
            config: None,
        };
        pack.manifest = read_json(&pack.inside("manifest.json")?)?;
        if let Some(path) = pack.manifest.get("config").and_then(Value::as_str) {
            let config = read_json(&pack.inside(path)?)?;
            pack.config = Some(config);
        }
        Ok(pack)
    }

    /// `relative` resolved inside the pack directory, following no link out of it.
    fn inside(&self, relative: &str) -> Result<PathBuf> {
        let candidate = self.dir.join(relative);
        let path = self
            .context
            .resolve_path_allow_create(&candidate.to_string_lossy())?;
        anyhow::ensure!(
            path.starts_with(&self.dir),
            "{relative} is outside the pack directory {}",
            self.dir.display()
        );
        Ok(path)
    }

    pub fn save(&self) -> Result<()> {
        write_json(&self.inside("manifest.json")?, &self.manifest)?;
        if let (Some(config), Some(path)) = (
            &self.config,
            self.manifest.get("config").and_then(Value::as_str),
        ) {
            write_json(&self.inside(path)?, config)?;
        }
        Ok(())
    }

    fn kind(&self) -> &str {
        self.manifest["kind"].as_str().unwrap_or_default()
    }

    pub fn config(&mut self) -> Result<&mut Map<String, Value>> {
        if self.config.is_none() {
            anyhow::ensure!(
                matches!(self.kind(), "assets" | "documents"),
                "a {} pack carries no configuration",
                self.kind()
            );
            self.manifest["kind"] = json!("documents");
            self.manifest["config"] = json!(CONFIG);
            self.add_asset(CONFIG);
            self.config = Some(json!({ "node": {} }));
        }
        self.config
            .as_mut()
            .and_then(Value::as_object_mut)
            .context("pack_config.json is not a JSON object")
    }

    pub fn list(&mut self, key: &str) -> Result<&mut Vec<Value>> {
        list_in(self.config()?, key)
    }

    pub fn manifest_list(&mut self, key: &str) -> Result<&mut Vec<Value>> {
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
        let file = self.inside(path)?;
        if file.is_file() {
            std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?;
        }
        Ok(())
    }

    pub fn create_file(&mut self, path: &str, contents: impl AsRef<[u8]>) -> Result<()> {
        let target = self.inside(path)?;
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

    /// Adds one part to the pack in memory and writes its files; `save` writes
    /// the manifest and configuration that name them.
    pub fn add(&mut self, addition: PackAddition) -> Result<()> {
        match addition {
            PackAddition::Agent { id, slot } => add_agent(self, &id, slot.as_deref()),
            PackAddition::Task { id, agent } => {
                check_id(&id)?;
                self.require_new("tasks", "task_id", &id)?;
                anyhow::ensure!(
                    self.has("agents", "agent_slot", &agent),
                    "the pack has no agent {agent:?}; add it first"
                );
                let prompt = format!("tasks/{}/prompt.md", snake(&id));
                self.create_file(&prompt, "Describe what this task does.\n")?;
                self.list("tasks")?.push(json!({
                    "task_id": id,
                    "display_name": title(&id),
                    "agent_did": crate::pack::agent_slot_reference(&agent),
                    "prompt_template": format!("./{prompt}"),
                }));
                Ok(())
            }
            PackAddition::Trigger { id, task, on } => {
                check_id(&id)?;
                self.require_new("triggers", "trigger_id", &id)?;
                self.require_new("event_sources", "event_source_id", &id)?;
                anyhow::ensure!(
                    self.has("tasks", "task_id", &task),
                    "the pack has no task {task:?}; add it first"
                );
                self.list("event_sources")?.push(json!({
                    "event_source_id": id,
                    "display_name": format!("{on} created"),
                    "source_collection": on,
                    "event_kind": "created",
                }));
                self.list("triggers")?.push(json!({
                    "trigger_id": id,
                    "display_name": title(&id),
                    "task_id": task,
                    "source": {"kind": "event", "event_source_id": id},
                    "concurrency": "serial",
                }));
                Ok(())
            }
            PackAddition::Schema { collection } => {
                anyhow::ensure!(
                    collection
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_uppercase())
                        && collection.chars().all(|c| c.is_ascii_alphanumeric()),
                    "{collection:?} is not a collection name; use PascalCase"
                );
                // An installed schema needs a documents pack; an assets pack
                // becomes one here, a plugins pack is refused.
                self.config()?;
                let path = format!("schemas/{}.graphql", super_snake(&collection));
                self.create_file(
                    &path,
                    format!("type {collection} {{\n  run_id: String @index\n}}\n"),
                )?;
                self.manifest_list("schemas")?.push(json!(path));
                Ok(())
            }
            PackAddition::Skill { id, from } => {
                check_id(&id)?;
                self.require_new("skills", "skill_id", &id)?;
                let body = match from {
                    Some(path) => std::fs::read_to_string(&path)
                        .with_context(|| format!("reading {}", path.display()))?,
                    None => format!("# {}\n\nWhen and how to use this skill.\n", title(&id)),
                };
                let path = format!("skills/{}/SKILL.md", snake(&id));
                self.create_file(&path, &body)?;
                self.list("skills")?.push(json!({
                    "skill_id": id,
                    "name": id,
                    "instructions": format!("./{path}"),
                }));
                Ok(())
            }
            PackAddition::Plugin {
                name,
                language,
                prebuilt,
            } => add_plugin(self, &name, language.as_deref(), prebuilt.as_deref()),
            PackAddition::Page { name } => add_page(self, &name),
        }
    }

    pub fn remove_part(&mut self, part: PackPart, id: &str) -> Result<()> {
        match part {
            PackPart::Agent => {
                let agent = self.remove_row("agents", "agent_slot", id)?;
                let reference = crate::pack::agent_slot_reference(id);
                anyhow::ensure!(
                    !self.config.as_ref().is_some_and(|config| config["tasks"]
                        .as_array()
                        .is_some_and(|tasks| tasks
                            .iter()
                            .any(|task| task["agent_did"] == reference))),
                    "a task still runs agent {id:?}; remove it first"
                );
                if let Some(context_id) = agent["context_id"].as_str() {
                    let context = self.remove_row("contexts", "context_id", context_id)?;
                    if let Some(tools_id) = context["tools_id"].as_str() {
                        let _ = self.remove_row("tools", "tools_id", tools_id);
                    }
                    if let Some(prompt) = context["system_prompt"].as_str() {
                        self.remove_asset(prompt.trim_start_matches("./"))?;
                    }
                }
                for slot in self.manifest_list("inference_slots")? {
                    if let Some(agents) = slot["agents"].as_array_mut() {
                        agents.retain(|agent| agent != id);
                    }
                }
                self.manifest_list("inference_slots")?.retain(|slot| {
                    slot["optional"] == true
                        || slot["agents"].as_array().is_some_and(|b| !b.is_empty())
                });
            }
            PackPart::Task => {
                let task = self.remove_row("tasks", "task_id", id)?;
                if let Some(prompt) = task["prompt_template"].as_str() {
                    self.remove_asset(prompt.trim_start_matches("./"))?;
                }
            }
            PackPart::Trigger => {
                let trigger = self.remove_row("triggers", "trigger_id", id)?;
                if let Some(source) = trigger["source"]["event_source_id"].as_str() {
                    let _ = self.remove_row("event_sources", "event_source_id", source);
                }
            }
            PackPart::Skill => {
                let skill = self.remove_row("skills", "skill_id", id)?;
                if let Some(path) = skill["instructions"].as_str() {
                    if path.starts_with("./") {
                        self.remove_asset(path.trim_start_matches("./"))?;
                    }
                }
            }
            PackPart::Schema => {
                let path = format!("schemas/{}.graphql", super_snake(id));
                anyhow::ensure!(
                    self.manifest["assets"]
                        .as_array()
                        .is_some_and(|assets| assets.iter().any(|asset| asset == &json!(path))),
                    "the pack has no schema {id:?}"
                );
                self.remove_asset(&path)?;
            }
            PackPart::Plugin => {
                let plugins = self.manifest_list("plugins")?;
                let index = plugins
                    .iter()
                    .position(|plugin| plugin["name"] == id)
                    .with_context(|| format!("the pack has no plugin {id:?}"))?;
                let plugin = plugins.remove(index);
                if let Some(artifact) = plugin["artifact"].as_str() {
                    self.remove_asset(artifact)?;
                }
                if let Some(instructions) = plugin["instructions"].as_str() {
                    self.remove_asset(instructions)?;
                    let path = self.inside(instructions)?;
                    if path.is_file() {
                        std::fs::remove_file(&path)
                            .with_context(|| format!("removing {}", path.display()))?;
                    }
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::remove_dir(dir);
                    }
                }
                if let Some(source) = plugin["source"].as_str() {
                    let source = self.inside(source)?;
                    if source.is_dir() {
                        std::fs::remove_dir_all(&source)
                            .with_context(|| format!("removing {}", source.display()))?;
                    }
                }
            }
        }
        Ok(())
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

fn add_agent(pack: &mut PackEdit, id: &str, slot: Option<&str>) -> Result<()> {
    check_id(id)?;
    pack.require_new("agents", "agent_slot", id)?;
    let slot = slot.map(str::to_owned).unwrap_or_else(|| snake(id));
    let prompt = format!("agents/{}/system_prompt.md", snake(id));
    pack.create_file(&prompt, format!("You are the {} agent.\n", title(id)))?;
    pack.list("agents")?.push(json!({
        "agent_slot": id,
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
            "agents",
        )?
        .push(json!(id)),
        None => slots.push(json!({
            "name": slot,
            "description": format!("The profile the {} agent runs on.", title(id)),
            "agents": [id],
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
            let afb = afterburner_afb::Afb::from_bytes(&bytes)
                .with_context(|| format!("{} is not a compiled plugin", file.display()))?;
            entry["language"] = json!(afb.manifest.package.language);
            let target = pack.inside(&artifact)?;
            anyhow::ensure!(!target.exists(), "{artifact} already exists");
            std::fs::create_dir_all(target.parent().context("artifact parent")?)?;
            std::fs::write(&target, bytes).with_context(|| format!("writing {artifact}"))?;
        }
        None => {
            let source = pack.inside(&format!("plugins/{name}"))?;
            let names_language = write_plugin_source(&source, name, language)?;
            entry["language"] = json!(names_language);
            entry["source"] = json!(format!("plugins/{name}"));
        }
    }
    let (instructions, markdown) = tool_markdown(name);
    let instructions_path = pack.inside(&instructions)?;
    std::fs::create_dir_all(instructions_path.parent().context("instructions parent")?)?;
    std::fs::write(&instructions_path, markdown)
        .with_context(|| format!("writing {instructions}"))?;
    entry["instructions"] = json!(instructions);
    pack.add_asset(&instructions);
    pack.add_asset(&artifact);
    pack.manifest_list("plugins")?.push(entry);
    Ok(())
}

fn add_page(pack: &mut PackEdit, name: &str) -> Result<()> {
    let names = Names::new(
        name,
        pack.manifest["namespace"].as_str().unwrap_or_default(),
    )?;
    anyhow::ensure!(
        !pack.manifest["ux"]
            .as_array()
            .is_some_and(|ux| ux.iter().any(|page| page["name"] == name)),
        "the pack already has a page {name:?}"
    );
    let (path, source, entry) = ux_plugin(&names);
    pack.create_file(&path, source)?;
    pack.manifest_list("ux")?.push(entry);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::scaffold::{scaffold, ScaffoldInput};
    use super::super::template::Template;
    use super::*;

    async fn check(dir: &Path) -> Vec<String> {
        super::super::check::check_dir(dir).await.problems
    }

    fn scaffolded(template: Template) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold(
            &dir,
            "review_toolkit",
            &ScaffoldInput {
                kind: None,
                namespace: "acme".into(),
                template: Some(template),
                language: None,
                ux: false,
            },
        )
        .unwrap();
        (root, dir)
    }

    fn add(dir: &Path, addition: PackAddition) {
        let mut pack = PackEdit::load(dir).unwrap();
        pack.add(addition).unwrap();
        pack.save().unwrap();
    }

    fn remove(dir: &Path, part: PackPart, id: &str) {
        let mut pack = PackEdit::load(dir).unwrap();
        pack.remove_part(part, id).unwrap();
        pack.save().unwrap();
    }

    #[tokio::test]
    async fn parts_added_to_an_assets_pack_keep_it_valid_and_removal_restores_it() {
        let (_root, dir) = scaffolded(Template::Assets);
        add(
            &dir,
            PackAddition::Agent {
                id: "reviewer".into(),
                slot: None,
            },
        );
        add(
            &dir,
            PackAddition::Task {
                id: "review".into(),
                agent: "reviewer".into(),
            },
        );
        add(
            &dir,
            PackAddition::Schema {
                collection: "ReviewJob".into(),
            },
        );
        add(
            &dir,
            PackAddition::Trigger {
                id: "on-job".into(),
                task: "review".into(),
                on: "ReviewJob".into(),
            },
        );
        add(
            &dir,
            PackAddition::Skill {
                id: "triage".into(),
                from: None,
            },
        );
        let problems = check(&dir).await;
        assert!(problems.is_empty(), "{problems:#?}");

        remove(&dir, PackPart::Trigger, "on-job");
        remove(&dir, PackPart::Skill, "triage");
        remove(&dir, PackPart::Task, "review");
        remove(&dir, PackPart::Agent, "reviewer");
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

    /// A schema on an assets pack makes it the documents pack an installed
    /// schema requires; with a page it needs no agent or inference slot.
    #[tokio::test]
    async fn a_schema_and_a_page_make_an_assets_pack_a_valid_documents_pack() {
        let (_root, dir) = scaffolded(Template::Assets);
        add(
            &dir,
            PackAddition::Schema {
                collection: "Note".into(),
            },
        );
        add(
            &dir,
            PackAddition::Page {
                name: "notes".into(),
            },
        );
        let problems = check(&dir).await;
        assert!(problems.is_empty(), "{problems:#?}");
        let manifest = read_json(&dir.join("manifest.json")).unwrap();
        assert_eq!(manifest["kind"], "documents");
        assert_eq!(manifest["ux"][0]["entry"], "ux/notes/plugin.js");
        assert!(dir.join("ux/notes/plugin.js").is_file());
    }

    #[test]
    fn duplicate_pages_are_refused() {
        let (_root, dir) = scaffolded(Template::Minimal);
        add(
            &dir,
            PackAddition::Page {
                name: "board".into(),
            },
        );
        let mut pack = PackEdit::load(&dir).unwrap();
        let duplicate = pack
            .add(PackAddition::Page {
                name: "board".into(),
            })
            .unwrap_err();
        assert!(
            format!("{duplicate:#}").contains("already has a page"),
            "{duplicate:#}"
        );
    }

    #[test]
    fn duplicates_and_dangling_references_are_refused() {
        let (_root, dir) = scaffolded(Template::Minimal);
        let mut pack = PackEdit::load(&dir).unwrap();
        let duplicate = pack
            .add(PackAddition::Agent {
                id: "review-toolkit-worker".into(),
                slot: None,
            })
            .unwrap_err();
        assert!(
            format!("{duplicate:#}").contains("already has"),
            "{duplicate:#}"
        );
        let dangling = pack
            .add(PackAddition::Task {
                id: "orphan".into(),
                agent: "nobody".into(),
            })
            .unwrap_err();
        assert!(format!("{dangling:#}").contains("no agent"), "{dangling:#}");
    }

    /// The manifest is authored, so its `config` path is untrusted input.
    #[test]
    fn a_manifest_config_outside_the_pack_is_refused() {
        let (root, dir) = scaffolded(Template::Assets);
        std::fs::write(root.path().join("outside.json"), "{\"secret\":true}\n").unwrap();
        let path = dir.join("manifest.json");
        let mut manifest = read_json(&path).unwrap();
        manifest["config"] = json!("../outside.json");
        write_json(&path, &manifest).unwrap();
        let error = PackEdit::load(&dir)
            .err()
            .expect("a config outside the pack is refused");
        assert!(format!("{error:#}").contains("outside"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_part_directory_is_refused_before_writing() {
        let (_root, dir) = scaffolded(Template::Assets);
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("ux")).unwrap();
        let mut pack = PackEdit::load(&dir).unwrap();
        let error = pack
            .add(PackAddition::Page {
                name: "board".into(),
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("outside"), "{error:#}");
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[test]
    fn fmt_is_idempotent_and_changes_no_meaning() {
        let (_root, dir) = scaffolded(Template::Automation);
        let before = read_json(&dir.join(CONFIG)).unwrap();
        PackEdit::load(&dir).unwrap().save().unwrap();
        let once = std::fs::read(dir.join(CONFIG)).unwrap();
        PackEdit::load(&dir).unwrap().save().unwrap();
        assert_eq!(std::fs::read(dir.join(CONFIG)).unwrap(), once);
        assert_eq!(read_json(&dir.join(CONFIG)).unwrap(), before);
    }
}
