//! The reference must never reject input the loaders accept. Every
//! `pack_config.json` and pack `manifest.json` in the repository is checked
//! as written, after each schema-declared property is set to null or
//! removed, and after each string is rewritten as a `${...}` substitution:
//! whenever the loader still accepts the result, the exported schema must too.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use gents::pack::interpolate::{interpolate_with, SUBSTITUTION_PATTERN};
use gents::pack::{
    authored_document_schema, authored_pack_config_schema, decode_pack_config,
    load_pack_authoring_config, PackInstallOptions, PackManifest,
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
    /// The same string written as a substitution of an unset variable whose
    /// default is the original, which the loader reads back unchanged.
    Interpolate,
    /// Spellings a normalizing deserializer may read as an enumerated value.
    Spelling(Spelling),
}

#[derive(Clone, Copy, Debug)]
enum Spelling {
    Leading,
    Trailing,
    Padded,
    Blank,
    Whitespace,
    Upper,
    Lower,
}

impl Spelling {
    const ALL: [Self; 7] = [
        Self::Leading,
        Self::Trailing,
        Self::Padded,
        Self::Blank,
        Self::Whitespace,
        Self::Upper,
        Self::Lower,
    ];

    fn respell(self, text: &str) -> Option<String> {
        let respelled = match self {
            Self::Leading => format!(" {text}"),
            Self::Trailing => format!("{text}\t"),
            Self::Padded => format!("  {text}\n"),
            Self::Blank => String::new(),
            Self::Whitespace => "  ".to_owned(),
            Self::Upper => text.to_uppercase(),
            Self::Lower => text.to_lowercase(),
        };
        (respelled != text).then_some(respelled)
    }
}

#[derive(Debug)]
struct Mutation {
    parent: Vec<Step>,
    target: Step,
    edit: Edit,
}

impl Mutation {
    fn new(parent: &[Step], target: Step, edit: Edit) -> Self {
        Self {
            parent: parent.to_vec(),
            target,
            edit,
        }
    }

    fn pointer(&self) -> String {
        let mut pointer = String::new();
        for step in self.parent.iter().chain([&self.target]) {
            match step {
                Step::Key(key) => pointer.push_str(&format!("/{key}")),
                Step::Index(index) => pointer.push_str(&format!("/{index}")),
            }
        }
        format!("{pointer} {:?}", self.edit)
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
        if let (Step::Key(key), Edit::Remove) = (&self.target, self.edit) {
            node.as_object_mut()?.remove(key)?;
            return Some(value);
        }
        let slot = match &self.target {
            Step::Key(key) => node
                .as_object_mut()?
                .entry(key.clone())
                .or_insert(Value::Null),
            Step::Index(index) => node.get_mut(*index)?,
        };
        *slot = match self.edit {
            Edit::Null => Value::Null,
            Edit::Remove => return None,
            Edit::Interpolate => {
                let text = slot.as_str().filter(|text| !text.contains(['$', '}']))?;
                Value::String(format!("${{DOCS_BUNDLE_UNSET:-{text}}}"))
            }
            Edit::Spelling(spelling) => Value::String(spelling.respell(slot.as_str()?)?),
        };
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

    /// Every string may be interpolated; a string whose schema enumerates
    /// values is also respelled.
    fn string_edits(&self, schema: &'a Value, value: &Value) -> Vec<Edit> {
        if !value.is_string() {
            return Vec::new();
        }
        let mut edits = vec![Edit::Interpolate];
        if self.enumerates_strings(schema, 0) {
            edits.extend(Spelling::ALL.map(Edit::Spelling));
        }
        edits
    }

    fn enumerates_strings(&self, schema: &'a Value, depth: usize) -> bool {
        let schema = self.resolve(schema);
        let listed = schema["enum"]
            .as_array()
            .is_some_and(|values| values.iter().any(Value::is_string))
            || schema["const"].is_string();
        listed
            || (depth < 8
                && ["anyOf", "oneOf", "allOf"].iter().any(|key| {
                    schema[*key]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|branch| self.enumerates_strings(branch, depth + 1))
                }))
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
                out.push(Mutation::new(path, Step::Key(key.clone()), Edit::Null));
                if let Some(child) = object.get(key) {
                    out.push(Mutation::new(path, Step::Key(key.clone()), Edit::Remove));
                    for edit in self.string_edits(property, child) {
                        out.push(Mutation::new(path, Step::Key(key.clone()), edit));
                    }
                    let mut child_path = path.to_vec();
                    child_path.push(Step::Key(key.clone()));
                    self.mutations(property, child, &child_path, out);
                }
            }
        }
        if let (Some(items), Some(array)) = (schema.get("items"), instance.as_array()) {
            for (index, item) in array.iter().enumerate() {
                for edit in self.string_edits(items, item) {
                    out.push(Mutation::new(path, Step::Index(index), edit));
                }
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
    if config
        .get("agents")
        .and_then(Value::as_array)
        .is_some_and(|agents| !agents.is_empty())
    {
        let Some(manifest) = fs::read(dir.join("manifest.json"))
            .ok()
            .and_then(|bytes| PackManifest::from_slice(&bytes).ok())
        else {
            return false;
        };
        let Some(config_path) = manifest.config.as_deref() else {
            return false;
        };
        return load_pack_authoring_config(
            &manifest,
            &PackInstallOptions { node_did: owner },
            &|path| {
                if path == config_path {
                    return Ok(serde_json::to_vec(config)?);
                }
                if let Some(text) = sidecar {
                    return Ok(text.as_bytes().to_vec());
                }
                Ok(fs::read(dir.join(path))?)
            },
            &|_| None,
        )
        .is_ok();
    }
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

#[test]
fn authored_agent_schema_requires_slots_without_materialized_identity() {
    let root = repository_root();
    let path = root.join(DOCUMENTS_FIXTURE);
    let dir = path.parent().unwrap();
    let config = read_json(&path).expect("fixture config");
    assert!(decodes(&config, dir, None));
    let schema = authored_pack_config_schema();
    let validator = jsonschema::validator_for(schema.as_value()).expect("compiles");
    assert!(validator.is_valid(&config));
    let mut forged = config.clone();
    forged["agents"][0]["agent_did"] = json!("did:key:forged");
    assert!(!decodes(&forged, dir, None));
    assert!(!validator.is_valid(&forged));
    let mut missing = config;
    missing["agents"][0]
        .as_object_mut()
        .unwrap()
        .remove("agent_slot");
    assert!(!decodes(&missing, dir, None));
    assert!(!validator.is_valid(&missing));
}

fn backend(fields: Value) -> Value {
    let mut backend = json!({
        "backend_id": "backend",
        "name": "backend",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind": "unauthenticated"}
    });
    if let (Some(backend), Some(fields)) = (backend.as_object_mut(), fields.as_object()) {
        backend.extend(fields.clone());
    }
    json!({"node": {"node_did": OWNER}, "inference_backends": [backend]})
}

#[test]
fn interpolated_input_validates_exactly_where_the_loader_substitutes() {
    let schema = authored_pack_config_schema();
    let validator = jsonschema::validator_for(schema.as_value()).expect("compiles");
    let dir = Path::new(".");
    for accepted in [
        backend(json!({"provider_kind": "${PROVIDER:-OpenAiCompatible}"})),
        backend(json!({"auth": {"kind": "${AUTH:-unauthenticated}"}})),
        backend(json!({"openai_wire_api": "${WIRE:-}${API:-chat_completions}"})),
    ] {
        assert!(
            decodes(&accepted, dir, None),
            "the loader accepts {accepted}"
        );
        let errors: Vec<_> = validator
            .iter_errors(&accepted)
            .map(|error| format!("{error} at {}", error.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{accepted} is rejected: {errors:?}");
    }
    // Interpolation only rewrites strings, so numbers and booleans stay
    // literal; escaped or unclosed `${` is not a substitution.
    for rejected in [
        backend(json!({"max_concurrent": "${CONCURRENCY:-2}"})),
        backend(json!({"enabled": "${ENABLED:-true}"})),
        backend(json!({"provider_kind": "$${PROVIDER}"})),
        backend(json!({"provider_kind": "${PROVIDER"})),
    ] {
        assert!(
            !decodes(&rejected, dir, None),
            "the loader rejects {rejected}"
        );
        assert!(
            !validator.is_valid(&rejected),
            "the schema accepts {rejected}"
        );
    }
}

/// Every string over a small alphabet up to six characters: the pattern must
/// match exactly those the loader performs a substitution in.
#[test]
fn substitution_pattern_matches_the_loader_parser() {
    let validator =
        jsonschema::validator_for(&json!({"type": "string", "pattern": SUBSTITUTION_PATTERN}))
            .expect("pattern compiles");
    let alphabet = ['$', '{', '}', 'a', ':', '-'];
    let mut strings = vec![String::new()];
    let mut frontier = vec![String::new()];
    for _ in 0..6 {
        frontier = frontier
            .iter()
            .flat_map(|prefix| alphabet.iter().map(move |next| format!("{prefix}{next}")))
            .collect();
        strings.extend(frontier.iter().cloned());
    }
    let mismatches: Vec<_> = strings
        .iter()
        .filter(|text| {
            let substituted = Cell::new(false);
            let _ = interpolate_with(text, &|_| {
                substituted.set(true);
                None
            });
            substituted.get() != validator.is_valid(&json!(text))
        })
        .take(20)
        .collect();
    assert!(
        mismatches.is_empty(),
        "pattern and parser disagree on {mismatches:?}"
    );
}

fn surface_entry(kind: Value) -> Value {
    let query = kind
        .as_str()
        .is_some_and(|kind| matches!(kind.trim(), "query" | "read"));
    let fields = if query {
        json!(["name"])
    } else {
        json!([{"name": "name"}])
    };
    json!({"node": {"node_did": OWNER}, "datastore_tool_surfaces": [{
        "surface_id": "surface",
        "entries": [{"kind": kind, "tool_name": "tool", "collection": "Record", "fields": fields}]
    }]})
}

#[test]
fn surface_entry_kinds_validate_exactly_as_the_deserializer_reads_them() {
    let schema = authored_pack_config_schema();
    let validator = jsonschema::validator_for(schema.as_value()).expect("compiles");
    let dir = Path::new(".");
    for kind in [
        json!(""),
        json!("  "),
        json!("  create  "),
        json!("\twrite\n"),
        json!(" query "),
        json!("read\t"),
        json!(null),
    ] {
        let config = surface_entry(kind);
        assert!(decodes(&config, dir, None), "the loader accepts {config}");
        let errors: Vec<_> = validator
            .iter_errors(&config)
            .map(|error| format!("{error} at {}", error.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{config} is rejected: {errors:?}");
    }
    for kind in ["creat", "qu ery", "Query", "x"] {
        let config = surface_entry(json!(kind));
        assert!(!decodes(&config, dir, None), "the loader rejects {config}");
        assert!(!validator.is_valid(&config), "the schema accepts {config}");
    }
    let mismatches: Vec<_> = ('\0'..='\u{3000}')
        .chain(['\u{feff}', '\u{205f}', '\u{2028}', '\u{2029}'])
        .filter(|padding| {
            let config = surface_entry(json!(format!("{padding}query")));
            decodes(&config, dir, None) != validator.is_valid(&config)
        })
        .map(|padding| format!("U+{:04X}", u32::from(padding)))
        .take(20)
        .collect();
    assert!(
        mismatches.is_empty(),
        "the kind pattern and str::trim disagree on {mismatches:?}"
    );
}

/// schemars generates no `pattern` of its own, so every patterned member of
/// an authored schema is a helper beside the canonical values, and the
/// reference folds it by its note.
#[test]
fn every_helper_member_carries_a_note() {
    fn patterned<'v>(value: &'v Value, at: String, out: &mut Vec<(String, &'v Value)>) {
        match value {
            Value::Object(object) => {
                if object.contains_key("pattern") {
                    out.push((at.clone(), value));
                }
                for (key, child) in object {
                    if key != "enum" && key != "const" && key != "default" && key != "examples" {
                        patterned(child, format!("{at}/{key}"), out);
                    }
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    patterned(child, format!("{at}/{index}"), out);
                }
            }
            _ => {}
        }
    }
    let schemas: Vec<(String, Schema)> =
        std::iter::once(("pack_config".to_owned(), authored_pack_config_schema()))
            .chain(
                Collection::ALL.into_iter().map(|collection| {
                    (collection.to_string(), authored_document_schema(collection))
                }),
            )
            .collect();
    let mut notes = BTreeSet::new();
    for (name, schema) in &schemas {
        let root = schema.as_value();
        assert!(
            jsonschema::meta::is_valid(root),
            "{name} is not a valid draft 2020-12 schema"
        );
        jsonschema::validator_for(root).expect("compiles with the annotation");
        let mut members = Vec::new();
        patterned(root, String::new(), &mut members);
        for (at, member) in members {
            let note = member["x-gents-note"].as_str();
            assert!(
                note.is_some_and(|note| !note.trim().is_empty()),
                "{name}{at} has no x-gents-note: {member}"
            );
            notes.extend(note.map(str::to_owned));
        }
    }
    assert!(
        notes.iter().any(|note| note.contains("${VAR}")),
        "substitution members are annotated: {notes:?}"
    );
    assert!(
        notes.iter().any(|note| note.contains("trimmed")),
        "trimmed spelling members are annotated: {notes:?}"
    );
}
