//! The reference must never reject input the loaders accept. Every
//! `pack_config.json` and pack `manifest.json` in the repository is checked
//! as written and after each schema-declared property is set to null or
//! removed: whenever the loader still accepts the result, the exported
//! schema must too.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use gents::pack::{
    authored_document_schema, authored_pack_config_schema, decode_pack_config, PackInstallOptions,
    PackManifest,
};
use gents::Collection;
use schemars::{schema_for, Schema};
use serde_json::{json, Value};

const OWNER: &str = "did:key:docs-bundle-owner";
const DOCUMENTS_FIXTURE: &str =
    "crates/gents/tests/fixtures/packs/documents_fixture/pack_config.json";
const SIDECAR_CASE: &str = r#"{"case_id":"one","split":"train","stages":[{"stage_id":"s","prompt":"p","deadline_secs":60,"checks":[{"check":"finding_count","params":{"count":1},"tier":"acceptance"}]}]}"#;

#[derive(Clone, Debug)]
enum Step {
    Key(String),
    Index(usize),
}

#[derive(Clone, Copy, Debug)]
enum Edit {
    Null,
    Remove,
}

#[derive(Debug)]
struct Mutation {
    parent: Vec<Step>,
    key: String,
    edit: Edit,
}

impl Mutation {
    fn pointer(&self) -> String {
        let mut pointer = String::new();
        for step in &self.parent {
            match step {
                Step::Key(key) => pointer.push_str(&format!("/{key}")),
                Step::Index(index) => pointer.push_str(&format!("/{index}")),
            }
        }
        format!("{pointer}/{} {:?}", self.key, self.edit)
    }

    fn apply(&self, base: &Value) -> Option<Value> {
        let mut value = base.clone();
        let mut node = &mut value;
        for step in &self.parent {
            node = match step {
                Step::Key(key) => node.get_mut(key)?,
                Step::Index(index) => node.get_mut(*index)?,
            };
        }
        let object = node.as_object_mut()?;
        match self.edit {
            Edit::Null => {
                object.insert(self.key.clone(), Value::Null);
            }
            Edit::Remove => {
                object.remove(&self.key)?;
            }
        }
        Some(value)
    }
}

/// Pairs an instance with the schema nodes that describe it, so properties a
/// fixture leaves out are still exercised.
struct Walk<'a> {
    root: &'a Value,
    branches: RefCell<BTreeMap<String, bool>>,
}

impl<'a> Walk<'a> {
    fn new(root: &'a Value) -> Self {
        Self {
            root,
            branches: RefCell::new(BTreeMap::new()),
        }
    }

    fn resolve(&self, mut schema: &'a Value) -> &'a Value {
        while let Some(name) = schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
        {
            schema = &self.root["$defs"][name];
        }
        schema
    }

    fn branch_accepts(&self, branch: &Value, instance: &Value) -> bool {
        let key = format!("{branch}\u{0}{instance}");
        if let Some(accepted) = self.branches.borrow().get(&key) {
            return *accepted;
        }
        let mut schema = branch.as_object().cloned().unwrap_or_default();
        schema.insert("$defs".into(), self.root["$defs"].clone());
        let accepted = jsonschema::validator_for(&Value::Object(schema))
            .is_ok_and(|validator| validator.is_valid(instance));
        self.branches.borrow_mut().insert(key, accepted);
        accepted
    }

    fn mutations(
        &self,
        schema: &'a Value,
        instance: &Value,
        path: &[Step],
        out: &mut Vec<Mutation>,
    ) {
        let schema = self.resolve(schema);
        if let (Some(properties), Some(object)) = (
            schema.get("properties").and_then(Value::as_object),
            instance.as_object(),
        ) {
            for (key, property) in properties {
                out.push(Mutation {
                    parent: path.to_vec(),
                    key: key.clone(),
                    edit: Edit::Null,
                });
                if let Some(child) = object.get(key) {
                    out.push(Mutation {
                        parent: path.to_vec(),
                        key: key.clone(),
                        edit: Edit::Remove,
                    });
                    let mut child_path = path.to_vec();
                    child_path.push(Step::Key(key.clone()));
                    self.mutations(property, child, &child_path, out);
                }
            }
        }
        if let (Some(items), Some(array)) = (schema.get("items"), instance.as_array()) {
            for (index, item) in array.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(Step::Index(index));
                self.mutations(items, item, &child_path, out);
            }
        }
        for branch in schema["allOf"].as_array().into_iter().flatten() {
            self.mutations(branch, instance, path, out);
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(branch) = schema[key]
                .as_array()
                .into_iter()
                .flatten()
                .find(|branch| self.branch_accepts(branch, instance))
            {
                self.mutations(branch, instance, path, out);
            }
        }
    }
}

/// Records every way the schema rejects `base` or a mutation of it that
/// `accepts` takes.
fn conform(
    label: &str,
    base: &Value,
    schema: &Schema,
    accepts: &dyn Fn(&Value) -> bool,
    failures: &mut Vec<String>,
) -> usize {
    let root = schema.as_value();
    let validator = jsonschema::validator_for(root).expect("exported schema compiles");
    let rejection = |instance: &Value| {
        validator
            .iter_errors(instance)
            .next()
            .map(|error| format!("{} at {}", error, error.instance_path()))
    };
    if let Some(error) = rejection(base) {
        failures.push(format!("{label}: as written: {error}"));
    }
    let mut mutations = Vec::new();
    Walk::new(root).mutations(root, base, &[], &mut mutations);
    let mut seen = BTreeSet::new();
    let mut checked = 0;
    for mutation in mutations {
        if !seen.insert(mutation.pointer()) {
            continue;
        }
        let Some(mutated) = mutation.apply(base) else {
            continue;
        };
        if !accepts(&mutated) {
            continue;
        }
        checked += 1;
        if let Some(error) = rejection(&mutated) {
            failures.push(format!("{label}: {}: {error}", mutation.pointer()));
        }
    }
    checked
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn files_named(root: &Path, names: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, names: &[&str], out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !name.starts_with('.') && name != "target" && name != "node_modules" {
                    walk(&path, names, out);
                }
            } else if names.contains(&name.as_ref()) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for top in ["crates", "examples"] {
        walk(&root.join(top), names, &mut out);
    }
    out.sort();
    out
}

fn decodes(config: &Value, dir: &Path, sidecar: Option<&str>) -> bool {
    let owner = config
        .pointer("/node/node_did")
        .and_then(Value::as_str)
        .filter(|did| !did.contains('$'))
        .unwrap_or(OWNER)
        .to_owned();
    decode_pack_config(
        config.clone(),
        Some(&PackInstallOptions { node_did: owner }),
        &|_| None,
        &|_, _, reference| {
            if let Some(text) = sidecar {
                return Ok(text.to_owned());
            }
            let relative = reference.strip_prefix("./").context("sidecar is not ./")?;
            fs::read_to_string(dir.join(relative)).with_context(|| format!("reading {reference}"))
        },
    )
    .is_ok()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

#[test]
fn every_repository_pack_the_loaders_accept_validates_against_the_reference() {
    let root = repository_root();
    let pack_schema = authored_pack_config_schema();
    let manifest_schema = schema_for!(PackManifest);
    let documents: Vec<_> = Collection::ALL
        .into_iter()
        .map(|collection| {
            let schema = authored_document_schema(collection);
            let validator = jsonschema::validator_for(schema.as_value()).expect("compiles");
            (collection, validator)
        })
        .collect();
    let mut failures = Vec::new();

    let mut configs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut manifests = 0;
    let mut manifest_mutations = 0;
    for path in files_named(&root, &["manifest.json", "pack_config.json"]) {
        if path
            .file_name()
            .is_some_and(|name| name == "pack_config.json")
        {
            configs.insert(path);
            continue;
        }
        let Some(manifest) = read_json(&path) else {
            continue;
        };
        let parses = |value: &Value| {
            serde_json::to_vec(value).is_ok_and(|bytes| PackManifest::from_slice(&bytes).is_ok())
        };
        if !parses(&manifest) {
            continue;
        }
        manifests += 1;
        if let Some(config) = manifest.get("config").and_then(Value::as_str) {
            if let Some(dir) = path.parent() {
                configs.insert(dir.join(config));
            }
        }
        let label = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        manifest_mutations += conform(&label, &manifest, &manifest_schema, &parses, &mut failures);
    }

    let mut accepted = BTreeSet::new();
    let mut config_mutations = 0;
    for path in &configs {
        let Some(config) = read_json(path) else {
            continue;
        };
        let dir = path.parent().expect("fixture directory");
        if !decodes(&config, dir, None) {
            continue;
        }
        let label = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        config_mutations += conform(
            &label,
            &config,
            &pack_schema,
            &|value| decodes(value, dir, None),
            &mut failures,
        );
        for (collection, validator) in &documents {
            let rows = match collection.dir_name() {
                Some(root) => config[root].as_array().cloned().unwrap_or_default(),
                None => vec![config["node"].clone()],
            };
            for (index, row) in rows.iter().enumerate() {
                if let Some(error) = validator.iter_errors(row).next() {
                    failures.push(format!(
                        "{label}: {collection}[{index}] against its document schema: {error} at {}",
                        error.instance_path()
                    ));
                }
            }
        }
        accepted.insert(label);
    }

    assert!(
        accepted.contains(DOCUMENTS_FIXTURE),
        "{DOCUMENTS_FIXTURE} must load; accepted: {accepted:?}"
    );
    assert!(
        failures.is_empty(),
        "{} rejections across {} pack configs ({config_mutations} accepted mutations) and {manifests} manifests ({manifest_mutations} accepted mutations):\n{}",
        failures.len(),
        accepted.len(),
        failures.join("\n")
    );
}

#[test]
fn null_owner_and_sidecar_input_the_loader_accepts_validates() {
    let schema = authored_pack_config_schema();
    let validator = jsonschema::validator_for(schema.as_value()).expect("compiles");
    let dir = Path::new(".");
    for (config, sidecar) in [
        (json!({"node": {"node_did": OWNER}, "tools": null}), None),
        (
            json!({"node": {}, "agents": [{
                "agent_id": "agent",
                "inference_profile_id": "profile",
                "tags": null,
                "enabled": null
            }]}),
            None,
        ),
        (
            json!({"node": {}, "eval_definitions": [{
                "definition_id": "definition",
                "comparability_version": 1,
                "subject": {"kind": "agent"},
                "cases": ["./cases/one.json"]
            }]}),
            Some(SIDECAR_CASE),
        ),
    ] {
        assert!(
            decodes(&config, dir, sidecar),
            "the loader accepts {config}"
        );
        let errors: Vec<_> = validator
            .iter_errors(&config)
            .map(|error| format!("{error} at {}", error.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{config} is rejected: {errors:?}");
    }
}
