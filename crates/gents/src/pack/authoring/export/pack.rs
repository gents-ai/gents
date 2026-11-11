use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};

use super::root::extract_sidecars;
pub use super::select::PulledByReference;
use super::select::{built_here, close, pack_part, Closure, ExportSelection, PackPart};
use crate::documents::pack_spec::is_valid_pack_name;
use crate::pack::authoring::{
    add_plugin_source, check_dir, pack_name, scaffold, to_snake_case, CheckReport, Names, PackEdit,
    ScaffoldInput, Template,
};
use crate::pack::{
    agent_did_variable, agent_slot_reference, inference_slot_reference, pack_origin_from_tags,
};
use crate::store::config_client::ConfigAccess;
use crate::Collection;

/// The variable every pack loader binds to the installing node's DID.
const NODE_DID_PLACEHOLDER: &str = "${GENTS_PACK_NODE_DID}";
/// The fields `bind_agent_references` rewrites from an agent slot reference.
const AGENT_REFERENCE_FIELDS: [&str; 4] = [
    "agent_did",
    "default_agent_did",
    "writer_agent_did",
    "integrator_agent_did",
];

#[derive(Debug, Serialize)]
pub struct ExportReport {
    pub pack: String,
    pub namespace: String,
    pub dir: PathBuf,
    /// The parts the selection named, as `Kind/ID` and `collection/Name`.
    pub selected: Vec<String>,
    /// The parts that came along because a selected part references them.
    pub by_reference: Vec<PulledByReference>,
    /// Each exported agent's slot and the live agent it was written from.
    pub agents: BTreeMap<String, String>,
    /// Each inference slot, the live profile it stands for and its agents.
    pub inference_slots: Vec<Value>,
    pub schemas: Vec<String>,
    /// The plugins the pack ships, each built by `pack build` from the source
    /// of the `node/NAME` plugin an exported Tools selected.
    pub plugins: Vec<String>,
    pub files: Vec<String>,
    pub check: CheckReport,
}

struct Plan {
    name: String,
    namespace: String,
    closure: Closure,
    schemas: BTreeMap<String, String>,
}

/// Writes the selected live documents of `node_did`, and what they reference,
/// to a new pack directory at `dir`, then checks it. Nothing on the node
/// changes.
pub async fn export_pack(
    access: &ConfigAccess,
    node_did: &str,
    dir: &Path,
    name: &str,
    namespace: &str,
    selection: &ExportSelection,
) -> Result<ExportReport> {
    let name = pack_name(name);
    Names::new(&name, namespace)?;
    anyhow::ensure!(
        !dir.exists(),
        "{} already exists; pack export writes a new pack directory",
        dir.display()
    );
    anyhow::ensure!(
        is_valid_pack_name(namespace),
        "pack namespaces are snake_case, not {namespace:?}"
    );
    let snapshot = access
        .transact_readonly("pack.export", |txn| {
            Box::pin(async move {
                super::snapshot::read_owned_documents_in_txn(txn, node_did, &|collection| {
                    pack_part(collection).is_some()
                })
                .await
            })
        })
        .await?;
    let closure = close(node_did, &snapshot, selection)?;
    let mut schemas = BTreeMap::new();
    for collection in &closure.collections {
        schemas.insert(
            collection.clone(),
            super::schema::collection_sdl(access, collection).await?,
        );
    }
    let plan = Plan {
        name,
        namespace: namespace.to_owned(),
        closure,
        schemas,
    };
    let owner = node_did.to_owned();
    let target = dir.to_path_buf();
    let written = tokio::task::spawn_blocking(move || write(&target, &owner, plan))
        .await
        .context("pack export task did not finish")??;
    let check = check_dir(&written.dir).await;
    Ok(ExportReport {
        pack: written.pack,
        namespace: written.namespace,
        dir: written.dir,
        selected: written.selected,
        by_reference: written.by_reference,
        agents: written.agents,
        inference_slots: written.inference_slots,
        schemas: written.schemas,
        plugins: written.plugins,
        files: written.files,
        check,
    })
}

struct Written {
    pack: String,
    namespace: String,
    dir: PathBuf,
    selected: Vec<String>,
    by_reference: Vec<PulledByReference>,
    agents: BTreeMap<String, String>,
    inference_slots: Vec<Value>,
    schemas: Vec<String>,
    plugins: Vec<String>,
    files: Vec<String>,
}

fn kebab(text: &str) -> String {
    to_snake_case(text).replace('_', "-")
}

fn unique(candidate: String, taken: &mut BTreeSet<String>, separator: char) -> String {
    let mut name = candidate.clone();
    let mut suffix = 2;
    while !taken.insert(name.clone()) {
        name = format!("{candidate}{separator}{suffix}");
        suffix += 1;
    }
    name
}

/// A sidecar directory name: snake_case, as a declared pack asset must be.
fn asset_handle(id: &str) -> String {
    let handle = to_snake_case(id);
    if is_valid_pack_name(&handle) {
        handle
    } else {
        format!("x_{handle}").trim_end_matches('_').to_owned()
    }
}

fn rewrite_agent_references(value: &mut Value, slots: &BTreeMap<String, String>) {
    match value {
        Value::Object(object) => {
            for (field, value) in object {
                if AGENT_REFERENCE_FIELDS.contains(&field.as_str()) {
                    if let Some(slot) = value.as_str().and_then(|did| slots.get(did)) {
                        *value = agent_slot_reference(slot).into();
                        continue;
                    }
                }
                rewrite_agent_references(value, slots);
            }
        }
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| rewrite_agent_references(value, slots)),
        _ => {}
    }
}

fn replace_strings(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(text) => *text = text.replace(from, to),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| replace_strings(value, from, to)),
        Value::Object(object) => object
            .values_mut()
            .for_each(|value| replace_strings(value, from, to)),
        _ => {}
    }
}

fn write(dir: &Path, node_did: &str, plan: Plan) -> Result<Written> {
    let Plan {
        name,
        namespace,
        closure,
        schemas,
    } = plan;

    let mut slot_names = BTreeSet::new();
    let mut inference_names = BTreeSet::new();
    let mut agent_slots: BTreeMap<String, String> = BTreeMap::new();
    let mut inference: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for ((collection, did), agent) in &closure.documents {
        if *collection != Collection::Agent {
            continue;
        }
        let mut slot = kebab(agent["display_name"].as_str().unwrap_or_default());
        if !slot.starts_with(|first: char| first.is_ascii_lowercase()) {
            slot = format!("agent-{slot}").trim_end_matches('-').to_owned();
        }
        let slot = unique(slot, &mut slot_names, '-');
        let profile = agent["inference_profile_id"]
            .as_str()
            .context("a live agent has no inference profile")?;
        if !inference.contains_key(profile) {
            // A profile ID that names a DID is this node's, so the slot is
            // named for the first agent that runs on it.
            let mut candidate = to_snake_case(profile);
            if profile.contains("did:") || !is_valid_pack_name(&candidate) {
                candidate = to_snake_case(&slot);
            }
            let candidate = unique(candidate, &mut inference_names, '_');
            inference.insert(profile.to_owned(), (candidate, Vec::new()));
        }
        inference
            .get_mut(profile)
            .expect("inserted above")
            .1
            .push(slot.clone());
        agent_slots.insert(did.clone(), slot);
    }

    let mut config = Map::new();
    for ((collection, id), document) in &closure.documents {
        if *collection == Collection::PluginSource && closure.plugins.contains(id) {
            // Shipped as the pack's plugin, whose source directory is the
            // one copy of this document.
            continue;
        }
        let Some(PackPart::Documents(list)) = pack_part(*collection) else {
            anyhow::bail!(
                "{} {id} is not a kind a pack carries",
                collection.graphql_type()
            );
        };
        let mut row = document
            .as_object()
            .context("a configuration document is not an object")?
            .clone();
        row.remove("node_did");
        row.retain(|_, value| !value.is_null());
        if let Some(tags) = row.get("tags").and_then(Value::as_array) {
            let tags: Vec<String> = tags
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .filter(|tag| {
                    pack_origin_from_tags(std::slice::from_ref(tag))
                        .ok()
                        .flatten()
                        .is_none()
                })
                .collect();
            if tags.is_empty() {
                row.remove("tags");
            } else {
                row.insert("tags".into(), json!(tags));
            }
        }
        if *collection == Collection::Skill {
            // A host directory, not portable to the installing node.
            row.remove("source_directory");
        }
        if *collection == Collection::Tools {
            // The pack ships the plugin under its own namespace, and a pin
            // names an artifact only this node built.
            for entry in row
                .get_mut("integrations")
                .and_then(|integrations| integrations.get_mut("plugins"))
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object_mut)
            {
                let shipped = entry["plugin"]
                    .as_str()
                    .and_then(built_here)
                    .filter(|name| closure.plugins.contains(*name))
                    .map(|name| format!("{namespace}/{name}"));
                if let Some(shipped) = shipped {
                    entry.insert("plugin".into(), json!(shipped));
                    entry.remove("digest");
                }
            }
        }
        if *collection == Collection::Agent {
            let slot = &agent_slots[id];
            let profile = row["inference_profile_id"].as_str().unwrap_or_default();
            let inference_slot = inference_slot_reference(&inference[profile].0);
            row.remove("agent_did");
            row.remove("creation_nonce");
            row.insert("agent_slot".into(), json!(slot));
            row.insert("inference_profile_id".into(), json!(inference_slot));
        }
        config
            .entry(list)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("list inserted above")
            .push(Value::Object(row));
    }
    let mut config = Value::Object(config);
    rewrite_agent_references(&mut config, &agent_slots);

    let mut handles: BTreeMap<(Collection, &str), String> = BTreeMap::new();
    let mut taken = BTreeSet::new();
    for (collection, id) in closure.documents.keys() {
        let directory = collection.dir_name().unwrap_or_default();
        let mut named = id.clone();
        for (did, slot) in &agent_slots {
            named = named.replace(did, slot);
        }
        let path = unique(
            format!("{directory}/{}", asset_handle(&named)),
            &mut taken,
            '_',
        );
        handles.insert(
            (*collection, id.as_str()),
            path[directory.len() + 1..].to_owned(),
        );
    }
    let mut sidecars: Vec<(String, String)> = Vec::new();
    extract_sidecars(
        &mut config,
        &|collection, id| handles[&(collection, id)].clone(),
        &mut |relative, text| {
            sidecars.push((relative.to_owned(), text.to_owned()));
            Ok(())
        },
    )?;
    // The loader interpolates every pack_config.json string, so text is
    // escaped to read back as itself before the pack's variables go in.
    crate::pack::interpolate::escape_values(&mut config);
    // An ID derived from an agent's DID (`<did>:context`) follows the slot.
    for (did, slot) in &agent_slots {
        replace_strings(&mut config, did, &agent_did_variable(slot));
    }
    replace_strings(&mut config, node_did, NODE_DID_PLACEHOLDER);
    let node_specific: Vec<&str> = sidecars
        .iter()
        .filter(|(_, text)| {
            text.contains(node_did) || agent_slots.keys().any(|did| text.contains(did))
        })
        .map(|(relative, _)| relative.as_str())
        .collect();
    anyhow::ensure!(
        node_specific.is_empty(),
        "these prompts name this node's DIDs, which no other node has; reword them and export again: {}",
        node_specific.join(", ")
    );

    let report = scaffold(
        dir,
        &name,
        &ScaffoldInput {
            namespace: namespace.clone(),
            template: Some(Template::Assets),
            kind: None,
            language: None,
            ux: false,
        },
    )?;
    let mut pack = PackEdit::load(&report.dir)?;
    for (relative, text) in &sidecars {
        pack.create_file(relative, text)?;
    }
    for (list, rows) in config.as_object().into_iter().flatten() {
        pack.list(list)?
            .extend(rows.as_array().into_iter().flatten().cloned());
    }
    let mut schema_paths = Vec::new();
    for (collection, sdl) in &schemas {
        // An installed schema needs a documents pack even with no document.
        pack.config()?;
        let path = format!("schemas/{}.graphql", asset_handle(collection));
        pack.create_file(&path, sdl)?;
        pack.manifest_list("schemas")?.push(json!(path));
        schema_paths.push(path);
    }
    let mut plugins = Vec::new();
    for id in &closure.plugins {
        let document = &closure.documents[&(Collection::PluginSource, id.clone())];
        add_plugin_source(&mut pack, &serde_json::from_value(document.clone())?)?;
        plugins.push(format!("{namespace}/{id}"));
    }
    let inference_slots: Vec<Value> = inference
        .values()
        .map(|(slot, agents)| {
            json!({
                "name": slot,
                "description": format!("The model {} runs on.", agents.join(", ")),
                "agents": agents,
            })
        })
        .collect();
    if !inference_slots.is_empty() {
        pack.manifest_list("inference_slots")?
            .extend(inference_slots.iter().cloned());
    }
    pack.manifest["description"] = json!(format!("{} pack, exported from a live node.", name));
    pack.save()?;
    std::fs::write(
        pack.dir.join("README.md"),
        readme(&name, &config, &inference_slots, &schema_paths),
    )
    .context("writing README.md")?;

    Ok(Written {
        pack: name,
        namespace,
        dir: pack.dir.clone(),
        selected: closure.selected,
        by_reference: closure.pulled,
        agents: agent_slots
            .into_iter()
            .map(|(did, slot)| (slot, did))
            .collect(),
        inference_slots,
        schemas: schema_paths,
        plugins,
        files: pack.manifest["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    })
}

fn readme(name: &str, config: &Value, slots: &[Value], schemas: &[String]) -> String {
    let mut documents = String::new();
    for collection in Collection::ALL {
        let Some(PackPart::Documents(list)) = pack_part(collection) else {
            continue;
        };
        for row in config[list].as_array().into_iter().flatten() {
            let id = row[collection.unique_field()]
                .as_str()
                .or(row["agent_slot"].as_str())
                .unwrap_or_default();
            documents.push_str(&format!("- {} `{id}`\n", collection.graphql_type()));
        }
    }
    let mut bindings = String::new();
    for slot in slots {
        bindings.push_str(&format!(
            "- `{}`: {}\n",
            slot["name"].as_str().unwrap_or_default(),
            slot["description"].as_str().unwrap_or_default()
        ));
    }
    if bindings.is_empty() {
        bindings.push_str("None.\n");
    }
    let mut collections = String::new();
    for schema in schemas {
        collections.push_str(&format!("- `{schema}`\n"));
    }
    if collections.is_empty() {
        collections.push_str("None.\n");
    }
    format!(
        "# {name}\n\nExported from documents that were live on a node.\n\n\
         ## Installation\n\n```sh\ngents config pack install ./{name}\n```\n\n\
         ## Bindings and prerequisites\n\nBind each inference slot to a profile when installing.\n\n{bindings}\n\
         ## Documents\n\n{documents}\n\
         ## Collections\n\n{collections}\n\
         ## Validation\n\n```sh\ngents config pack check ./{name}\n```\n"
    )
}
