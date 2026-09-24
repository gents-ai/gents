//! The subject dossier: what the author knows about the behavior it drafts
//! cases for. The author reads nothing itself; this is rendered once, from
//! the pack's own declared assets through the real pack loader.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use gents::document_config::{AgentBehavior, PackConfig};
use gents::pack::{
    declared_paths, digest_declared_assets, interpolate, load_pack_config, PackInstallOptions,
    PackManifest,
};
use serde_json::Value;

/// Everything the author may know about the subject, and the collections a
/// draft's documents captures may name.
pub(crate) struct Dossier {
    pub(crate) pack_name: String,
    pub(crate) pack_version: String,
    pub(crate) pack_digest: String,
    pub(crate) behavior_id: String,
    pub(crate) slot: String,
    /// Collections a documents capture may name, with their fields: from
    /// datastore surfaces (`fields` of each entry) and `schemas/*.graphql`
    /// (field names parsed from `type X { … }`).
    pub(crate) collections: BTreeMap<String, BTreeSet<String>>,
    pub(crate) text: String,
}

/// A dossier larger than this is refused: it would crowd the author's turn.
pub(crate) const DOSSIER_LIMIT_BYTES: usize = 64 * 1024;

/// The dossier of `behavior` (or the pack's only behavior) of the directory
/// pack at `pack_dir`.
pub(crate) fn render(pack_dir: &Path, behavior: Option<&str>) -> Result<Dossier> {
    let (manifest, assets) = read_pack(pack_dir)?;
    let asset = |path: &str| {
        assets
            .get(path)
            .with_context(|| format!("pack has no asset {path:?}"))
    };
    let pack_digest = digest_declared_assets(&manifest, |path| asset(path).map(Vec::as_slice))?;
    let config_path = manifest.config.clone().unwrap_or_default();

    // The loader fills `${VAR:-default}` with its default and refuses an
    // unset `${VAR}`; the dossier shows the marker instead. Every `$` of the
    // config's strings is escaped so interpolation reads each back as its own
    // text, except the owner marker the loader binds itself.
    let read = RefCell::new(BTreeSet::new());
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: "did:key:dossier".into(),
        },
        &|path| {
            read.borrow_mut().insert(path.to_owned());
            let bytes = asset(path)?.clone();
            if path == config_path {
                literal_placeholders(&bytes)
            } else {
                Ok(bytes)
            }
        },
        &|_name| None,
    )
    .with_context(|| format!("loading pack {}", manifest.name))?;
    let read = read.into_inner();

    let chosen = choose_behavior(&config, behavior)?;
    let behavior_id = chosen.behavior_id.clone();
    let slot = manifest
        .metadata
        .inference_slots
        .iter()
        .find(|slot| slot.behaviors.contains(&behavior_id))
        .map(|slot| slot.name.clone())
        .with_context(|| {
            format!(
                "behavior {behavior_id:?} binds no inference slot of pack {}",
                manifest.name
            )
        })?;
    let context = chosen.context_id.as_deref().and_then(|id| {
        config
            .contexts
            .iter()
            .find(|context| context.context_id == id)
    });

    let mut collections: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut text = String::new();

    text.push_str("# Subject\n\n## Identity\n\n");
    let _ = writeln!(text, "- pack: {} {}", manifest.name, manifest.version);
    let _ = writeln!(text, "- digest: {pack_digest}");
    let _ = writeln!(
        text,
        "- behavior: {behavior_id} ({})",
        chosen.display_name.as_deref().unwrap_or("no display name")
    );
    let _ = writeln!(text, "- inference slot: {slot}");

    text.push_str("\n## System prompt\n\n");
    match context.and_then(|context| context.system_prompt.as_deref()) {
        Some(prompt) => {
            text.push_str(prompt);
            if !prompt.ends_with('\n') {
                text.push('\n');
            }
        }
        None => text.push_str("(none)\n"),
    }

    text.push_str("\n## Tools\n\n");
    let tools = context
        .and_then(|context| context.tools_id.as_deref())
        .and_then(|id| config.tools.iter().find(|tools| tools.tools_id == id));
    match tools {
        None => text.push_str("(no tools document)\n"),
        Some(tools) => {
            let _ = writeln!(text, "Tools document {}:", tools.tools_id);
            let host = tools.host.as_ref();
            // A mode at its default is omitted when serialized; the typed
            // value is read instead, so an explicit `Off` still shows.
            let mode = |mode: Option<Result<Value, serde_json::Error>>| -> Result<String> {
                Ok(match mode.transpose()? {
                    Some(Value::String(mode)) => mode,
                    Some(other) => other.to_string(),
                    None => "none".to_owned(),
                })
            };
            let files = host
                .and_then(|host| host.files.as_ref())
                .map(|files| serde_json::to_value(&files.mode));
            let bash = host
                .and_then(|host| host.bash.as_ref())
                .map(|bash| serde_json::to_value(&bash.mode));
            let _ = writeln!(text, "- files: {}", mode(files)?);
            let _ = writeln!(text, "- bash: {}", mode(bash)?);
            if let Some(root) = host.and_then(|host| host.root.as_deref()) {
                let _ = writeln!(text, "- workspace root: {root}");
            }
            let surface_ids = tools
                .datastore
                .as_ref()
                .and_then(|datastore| datastore.datastore_tool_surface_ids.as_deref())
                .unwrap_or_default();
            for surface_id in surface_ids {
                let surface = config
                    .datastore_tool_surfaces
                    .iter()
                    .find(|surface| &surface.surface_id == surface_id)
                    .with_context(|| {
                        format!(
                            "tools {} name an unknown datastore surface {surface_id:?}",
                            tools.tools_id
                        )
                    })?;
                let _ = writeln!(text, "\n### Datastore surface {surface_id}");
                for entry in surface.entries.iter().flatten() {
                    render_entry(&serde_json::to_value(entry)?, &mut text, &mut collections);
                }
            }
        }
    }

    text.push_str("\n## Tasks\n");
    let tasks: Vec<_> = config
        .tasks
        .iter()
        .filter(|task| task.behavior_id == behavior_id)
        .collect();
    if tasks.is_empty() {
        text.push_str("\n(none)\n");
    }
    for task in tasks {
        let _ = writeln!(text, "\n### Task {}\n", task.task_id);
        let variables = template_variables(&task.prompt_template);
        let _ = writeln!(
            text,
            "Variables: {}",
            if variables.is_empty() {
                "none".to_owned()
            } else {
                variables.into_iter().collect::<Vec<_>>().join(", ")
            }
        );
        for trigger in config
            .triggers
            .iter()
            .filter(|trigger| trigger.task_id == task.task_id)
        {
            let _ = writeln!(
                text,
                "Trigger {}: {}",
                trigger.trigger_id,
                serde_json::to_string(&trigger.source)?
            );
        }
        text.push_str("Prompt template:\n");
        text.push_str(&task.prompt_template);
        if !task.prompt_template.ends_with('\n') {
            text.push('\n');
        }
    }

    text.push_str("\n## Schemas\n");
    let schemas: Vec<&String> = manifest
        .metadata
        .assets
        .iter()
        .filter(|path| is_schema(path))
        .collect();
    if schemas.is_empty() {
        text.push_str("\n(none)\n");
    }
    for path in &schemas {
        let sdl = String::from_utf8(asset(path)?.clone())
            .with_context(|| format!("schema {path} is not UTF-8"))?;
        let _ = writeln!(text, "\n### {path}");
        text.push_str(&sdl);
        if !sdl.ends_with('\n') {
            text.push('\n');
        }
        for (collection, fields) in schema_types(&sdl) {
            collections.entry(collection).or_default().extend(fields);
        }
    }

    text.push_str("\n## Fixture files\n");
    let fixtures: Vec<&String> = manifest
        .metadata
        .assets
        .iter()
        .filter(|path| !read.contains(*path) && !is_schema(path) && !is_readme(path))
        .collect();
    if fixtures.is_empty() {
        text.push_str("(none)\n");
    }
    for path in fixtures {
        let _ = writeln!(text, "- {path}");
    }

    if text.len() > DOSSIER_LIMIT_BYTES {
        let (largest, size) = assets
            .iter()
            .filter(|(path, _)| *path != "manifest.json" && !is_readme(path))
            .map(|(path, bytes)| (path.as_str(), bytes.len()))
            .max_by_key(|(_, size)| *size)
            .unwrap_or(("manifest.json", 0));
        anyhow::bail!(
            "the subject dossier is {} bytes, over the {DOSSIER_LIMIT_BYTES}-byte limit; its largest asset is {largest} ({size} bytes)",
            text.len()
        );
    }

    Ok(Dossier {
        pack_name: manifest.name,
        pack_version: manifest.version,
        pack_digest,
        behavior_id,
        slot,
        collections,
        text,
    })
}

/// The directory pack's manifest and the bytes of every asset it declares:
/// the manifest read as `resolve_subject_pack` reads a directory's, the
/// assets those the pack's digest covers.
fn read_pack(pack_dir: &Path) -> Result<(PackManifest, BTreeMap<String, Vec<u8>>)> {
    let manifest_path = pack_dir.join("manifest.json");
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?,
    )
    .with_context(|| format!("parsing {}", manifest_path.display()))?;
    let mut assets = BTreeMap::new();
    for path in declared_paths(&manifest) {
        let file = pack_dir.join(&path);
        let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        assets.insert(path, bytes);
    }
    Ok((manifest, assets))
}

/// The pack config with every `$` of its string values escaped, so the
/// loader's interpolation reads each string back as its own text: a
/// `${VAR:-default}` stays that marker. The owner marker is left for the
/// loader to bind, as it does for any install.
fn literal_placeholders(bytes: &[u8]) -> Result<Vec<u8>> {
    const OWNER: &str = "${GENTS_PACK_AGENT_DID}";
    fn escape(value: &mut Value) {
        match value {
            Value::String(text) => {
                *text = interpolate::escape(text).replace(&interpolate::escape(OWNER), OWNER);
            }
            Value::Array(values) => values.iter_mut().for_each(escape),
            Value::Object(values) => values.values_mut().for_each(escape),
            _ => {}
        }
    }
    let mut value: Value = serde_json::from_slice(bytes).context("parsing pack config JSON")?;
    escape(&mut value);
    Ok(serde_json::to_vec(&value)?)
}

fn choose_behavior<'a>(
    config: &'a PackConfig,
    behavior: Option<&str>,
) -> Result<&'a AgentBehavior> {
    let ids = || {
        config
            .agent_behaviors
            .iter()
            .map(|behavior| behavior.behavior_id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match behavior {
        Some(id) => config
            .agent_behaviors
            .iter()
            .find(|behavior| behavior.behavior_id == id)
            .with_context(|| format!("the pack has no behavior {id:?}; it declares: {}", ids())),
        None => match config.agent_behaviors.as_slice() {
            [only] => Ok(only),
            [] => anyhow::bail!("the pack declares no behavior to evaluate"),
            _ => anyhow::bail!(
                "the pack declares several behaviors ({}); choose one with --behavior",
                ids()
            ),
        },
    }
}

/// One datastore surface entry as the author reads it, and its collection's
/// fields into `collections`.
fn render_entry(
    entry: &Value,
    text: &mut String,
    collections: &mut BTreeMap<String, BTreeSet<String>>,
) {
    let str_of = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or_default();
    let names = |key: &str| -> Vec<String> {
        entry
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|field| match field {
                Value::String(name) => Some(name.clone()),
                Value::Object(field) => {
                    field.get("name").and_then(Value::as_str).map(str::to_owned)
                }
                _ => None,
            })
            .collect()
    };
    let collection = str_of("collection");
    let fields = names("fields");
    let filters = names("filter_fields");
    let _ = write!(
        text,
        "- tool {} on {collection}: {}; fields {}",
        str_of("tool_name"),
        str_of("description"),
        if fields.is_empty() {
            "(all)".to_owned()
        } else {
            fields.join(", ")
        }
    );
    if !filters.is_empty() {
        let _ = write!(text, "; filters {}", filters.join(", "));
    }
    text.push('\n');
    collections
        .entry(collection.to_owned())
        .or_default()
        .extend(fields.into_iter().chain(filters));
}

/// The `{{ name }}` variables a prompt template expects, sorted.
fn template_variables(template: &str) -> BTreeSet<String> {
    let mut variables = BTreeSet::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name: String = after[..end]
            .trim()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        if !name.is_empty() {
            variables.insert(name);
        }
        rest = &after[end + 2..];
    }
    variables
}

fn is_schema(path: &str) -> bool {
    path.starts_with("schemas/") && path.ends_with(".graphql")
}

/// The pack's own prose: excluded, since prose invites testing the prose.
fn is_readme(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.to_ascii_uppercase().starts_with("README"))
}

/// The `type Name { field: … }` blocks of an SDL document, with each
/// block's field names.
fn schema_types(sdl: &str) -> Vec<(String, BTreeSet<String>)> {
    let mut types = Vec::new();
    let mut current: Option<(String, BTreeSet<String>)> = None;
    for line in sdl.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if let Some((name, fields)) = current.as_mut() {
            if line.starts_with('}') {
                types.push((std::mem::take(name), std::mem::take(fields)));
                current = None;
            } else if let Some((field, _)) = line.split_once(':') {
                let field = field.split('(').next().unwrap_or_default().trim();
                if !field.is_empty() {
                    fields.insert(field.to_owned());
                }
            }
        } else if let Some(rest) = line.strip_prefix("type ") {
            let name: String = rest
                .trim()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && line.ends_with('{') {
                current = Some((name, BTreeSet::new()));
            }
        }
    }
    types
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{json, Value};

    use super::*;

    fn canary_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gents/tests/fixtures/eval_runner/canary_pack")
    }

    /// A directory pack: `config` as `pack_config.json`, a README, and
    /// `assets` (path, text). Each behavior gets its own inference slot.
    fn write_pack(dir: &Path, config: &Value, assets: &[(&str, &str)]) {
        let mut paths = vec!["README.md".to_owned(), "pack_config.json".to_owned()];
        std::fs::write(dir.join("README.md"), "# a fixture pack\n").unwrap();
        for (path, text) in assets {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
            paths.push((*path).to_owned());
        }
        paths.sort();
        let slots: Vec<Value> = config["agent_behaviors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|behavior| {
                let id = behavior["behavior_id"].as_str().unwrap();
                json!({
                    "name": format!("slot_{id}"),
                    "description": "Runs it.",
                    "behaviors": [id],
                })
            })
            .collect();
        let manifest = json!({
            "manifest_version": 1,
            "name": "fixture_pack",
            "version": "0.1.0",
            "description": "A dossier fixture.",
            "authors": ["gents-ai contributors"],
            "kind": "documents",
            "assets": paths,
            "config": "pack_config.json",
            "inference_slots": slots,
        });
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("pack_config.json"),
            serde_json::to_vec_pretty(config).unwrap(),
        )
        .unwrap();
    }

    fn behavior(id: &str) -> Value {
        json!({
            "behavior_id": id,
            "display_name": format!("Behavior {id}"),
            "context_id": format!("{id}-context"),
            "inference_profile_id": format!("gents:inference-slot:slot_{id}"),
        })
    }

    fn context(id: &str, prompt: &str) -> Value {
        json!({
            "context_id": format!("{id}-context"),
            "system_prompt": prompt,
            "tools_id": format!("{id}-tools"),
        })
    }

    #[test]
    fn the_canary_dossier_names_the_behavior_its_prompt_and_its_tools() {
        let dossier = render(&canary_dir(), None).unwrap();
        assert_eq!(dossier.pack_name, "eval_canary");
        assert_eq!(dossier.pack_version, "1.0.0");
        assert!(dossier.pack_digest.starts_with("sha256:"));
        assert_eq!(dossier.behavior_id, "canary");
        assert_eq!(dossier.slot, "primary");
        let text = &dossier.text;
        assert!(text.contains("# Subject"));
        assert!(text.contains("## Identity"));
        assert!(text.contains("canary (Eval canary)"), "{text}");
        assert!(text.contains(&dossier.pack_digest), "{text}");
        assert!(text.contains("## System prompt"));
        assert!(text.contains("You are the eval-runner canary."), "{text}");
        assert!(text.contains("files: ReadOnly"), "{text}");
        assert!(text.contains("bash: none"), "{text}");
        assert!(text.contains("## Fixture files\n- notes.txt"), "{text}");
        assert!(
            !text.contains("# Eval-runner canary pack"),
            "the README is excluded"
        );
        assert!(dossier.collections.is_empty());
    }

    #[test]
    fn a_pack_with_two_behaviors_needs_a_choice_and_refuses_an_unknown_one() {
        let dir = tempfile::tempdir().unwrap();
        let config = json!({
            "agent_principal": {},
            "agent_behaviors": [behavior("a"), behavior("b")],
            "contexts": [context("a", "Prompt A."), context("b", "Prompt B.")],
            "tools": [
                {"tools_id": "a-tools", "host": {"files": {"mode": "ReadOnly"}}},
                {"tools_id": "b-tools", "host": {"bash": {"mode": "Off"}}},
            ],
        });
        write_pack(dir.path(), &config, &[]);

        let error = render(dir.path(), None).err().unwrap().to_string();
        assert!(error.contains("a") && error.contains("b"), "{error}");
        assert!(error.contains("--behavior"), "{error}");

        let error = render(dir.path(), Some("nope")).err().unwrap().to_string();
        assert!(error.contains("nope"), "{error}");

        let dossier = render(dir.path(), Some("b")).unwrap();
        assert_eq!(dossier.behavior_id, "b");
        assert_eq!(dossier.slot, "slot_b");
        assert!(dossier.text.contains("Prompt B."));
        assert!(!dossier.text.contains("Prompt A."));
        assert!(dossier.text.contains("bash: Off"), "{}", dossier.text);
    }

    #[test]
    fn surfaces_tasks_and_schemas_are_rendered_and_give_collections() {
        let dir = tempfile::tempdir().unwrap();
        let config = json!({
            "agent_principal": {},
            "agent_behaviors": [behavior("a")],
            "contexts": [context("a", "Prompt A.")],
            "tools": [{
                "tools_id": "a-tools",
                "datastore": {"datastore_tool_surface_ids": ["notes"]},
            }],
            "datastore_tool_surfaces": [{
                "surface_id": "notes",
                "entries": [{
                    "tool_name": "write_note",
                    "collection": "Note",
                    "description": "Write one note.",
                    "fields": [{"name": "note_id", "required": true}, {"name": "body", "required": true}],
                }],
            }],
            "tasks": [{
                "task_id": "summarize",
                "behavior_id": "a",
                "prompt_template": "Summarize {{ topic }} for {{reader}}.",
            }],
        });
        write_pack(
            dir.path(),
            &config,
            &[
                (
                    "schemas/finding.graphql",
                    "type Finding {\n  finding_id: String @index\n  title: String\n}\n",
                ),
                ("fixtures/input.txt", "hello"),
            ],
        );
        let dossier = render(dir.path(), None).unwrap();
        let text = &dossier.text;
        assert!(
            text.contains("- tool write_note on Note: Write one note.; fields note_id, body"),
            "{text}"
        );
        assert!(text.contains("### Task summarize"), "{text}");
        assert!(
            text.contains("Summarize {{ topic }} for {{reader}}."),
            "{text}"
        );
        assert!(text.contains("reader, topic"), "{text}");
        assert!(
            text.contains("### schemas/finding.graphql\ntype Finding {"),
            "{text}"
        );
        assert!(text.contains("- fixtures/input.txt"), "{text}");
        assert!(!text.contains("- schemas/finding.graphql"), "{text}");
        let fields = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect();
        assert_eq!(dossier.collections["Note"], fields(&["body", "note_id"]));
        assert_eq!(
            dossier.collections["Finding"],
            fields(&["finding_id", "title"])
        );
    }

    #[test]
    fn placeholders_stay_markers_and_the_size_limit_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let config = json!({
            "agent_principal": {},
            "agent_behaviors": [behavior("a")],
            "contexts": [context("a", "Prompt A.")],
            "tools": [{
                "tools_id": "a-tools",
                "host": {
                    "root": "${GENTS_EVAL_WORKSPACE_ROOT:-.}",
                    "files": {"mode": "ReadOnly"},
                },
            }],
        });
        write_pack(dir.path(), &config, &[]);
        let dossier = render(dir.path(), None).unwrap();
        assert!(
            dossier.text.contains("${GENTS_EVAL_WORKSPACE_ROOT:-.}"),
            "{}",
            dossier.text
        );

        let big = tempfile::tempdir().unwrap();
        let config = json!({
            "agent_principal": {},
            "agent_behaviors": [behavior("a")],
            "contexts": [context("a", "./prompts/a.md")],
            "tools": [{"tools_id": "a-tools"}],
        });
        let prompt = "x".repeat(70 * 1024);
        write_pack(big.path(), &config, &[("prompts/a.md", &prompt)]);
        let error = format!("{:#}", render(big.path(), None).err().unwrap());
        assert!(error.contains("prompts/a.md"), "{error}");
        assert!(error.contains(&DOSSIER_LIMIT_BYTES.to_string()), "{error}");
    }
}
