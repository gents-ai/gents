#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::Deserialize;

mod json_wire;
pub use json_wire::compact_model_json;

pub fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// A JSON object read as its pairs in document order, which a `BTreeMap` would sort.
pub fn ordered_pairs<'de, D, T>(deserializer: D) -> Result<Vec<(String, T)>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Pairs<T>(std::marker::PhantomData<T>);
    impl<'de, T> serde::de::Visitor<'de> for Pairs<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<(String, T)>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a JSON object")
        }
        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut pairs = Vec::new();
            while let Some(pair) = map.next_entry()? {
                pairs.push(pair);
            }
            Ok(pairs)
        }
    }
    deserializer.deserialize_map(Pairs(std::marker::PhantomData))
}

pub type LeanFeatureMatrix = BTreeMap<String, BTreeMap<String, LeanFeatureMatrixCell>>;

#[derive(Debug, Clone, Copy)]
pub struct LeanVocabulary<'a> {
    pub lean_file: &'a str,
    pub model: &'a str,
    pub namespace: &'a str,
    pub rust_source: &'a str,
    pub rust_values: &'a [&'a str],
}

impl LeanSurface {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentFacing => "agentFacing",
            Self::OperatorCli => "operatorCli",
            Self::OperatorUi => "operatorUi",
            Self::Api => "api",
            Self::RuntimeInternal => "runtimeInternal",
        }
    }
}

#[rustfmt::skip]
mod generated;
pub use generated::*;

pub mod canonical_wire;
pub mod claude_replay;
pub mod consumer_scan;
pub mod family;
mod plugin_job_wire;
mod request_input_wire;
mod session_turn_wire;
mod title;

/// Family types generated from `families.schema.json`, which Lean emits.
pub mod families {
    include!(concat!(env!("OUT_DIR"), "/families.rs"));
}

pub use plugin_job_wire::*;
pub use request_input_wire::*;
pub use session_turn_wire::*;
pub use title::*;

#[cfg(test)]
mod tests;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LeanVocabularyParseError<'a> {
    MissingNamespace,
    MissingToDefraDB,
    EmptyToDefraDB,
    MalformedArm {
        line_number: usize,
        line: &'a str,
        reason: &'static str,
    },
}

static LEAN_CONTRACT_SNAPSHOT: OnceLock<LeanContractSnapshot> = OnceLock::new();

pub fn lean_contract_snapshot() -> &'static LeanContractSnapshot {
    LEAN_CONTRACT_SNAPSHOT.get_or_init(load_lean_contract_snapshot)
}

fn lean_plugin_resource_group<T>(key: &str, cases: &serde_json::Value) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(cases.clone()).unwrap_or_else(|error| panic!("{key}: {error}"))
}

pub fn lean_recovery_sweep_case(name: &str) -> &'static LeanRecoverySweepCase {
    lean_contract_snapshot()
        .recovery_sweep_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean recovery sweep case {name:?} was not emitted"))
}

pub fn lean_transcript_case(name: &str) -> &'static LeanTranscriptCase {
    lean_contract_snapshot()
        .transcript_conformance_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean transcript case {name:?} was not emitted"))
}

#[derive(Debug, PartialEq, Eq)]
pub struct QueueImage {
    pub active: Option<String>,
    pub folding: Vec<String>,
    pub pending: Vec<String>,
    pub terminal: BTreeSet<String>,
}

pub fn assert_lean_to_defradb_vocabulary_matches(spec: LeanVocabulary<'_>) {
    let lean_values = lean_to_defradb_values(spec.lean_file, spec.model, spec.namespace);
    let missing_from_lean = values_missing_from(spec.rust_values, &lean_values);
    let extra_in_lean = values_missing_from(&lean_values, spec.rust_values);
    let duplicate_rust_values = duplicate_values(spec.rust_values);
    let duplicate_lean_values = duplicate_values(&lean_values);

    assert!(
        spec.rust_values == lean_values.as_slice()
            && missing_from_lean.is_empty()
            && extra_in_lean.is_empty()
            && duplicate_rust_values.is_empty()
            && duplicate_lean_values.is_empty(),
        "Rust/Lean toDefraDB vocabulary mismatch\n  Lean file: {}\n  namespace: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.lean_file,
        spec.namespace,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

/// Mirrors the conformance build target and generator of `gents_lean_contract`.
/// The proofs directory is anchored at compile time: the runtime
/// `CARGO_MANIFEST_DIR` lookup in `gents_lean_contract::proofs_dir` is absent
/// when a test binary runs outside cargo. The anchor names the `gents` crate,
/// which holds the proofs, as a sibling of this crate's manifest.
const LEAN_CONTRACT_TARGET: &str = "Proofs.Conformance.Contracts:olean";
const LEAN_CONTRACT_FILE: &str = "Proofs/Conformance/Contracts.lean";

/// A test that passes while the cases it reads are wrong compares nothing.
/// `GENTS_SNAPSHOT_MUTATION=<key>` flips every boolean and bumps every number
/// under that snapshot key, and `<key>.<field>` only those in fields of that
/// name; a reader of the key must then fail. Nothing in the repository sets
/// it: the conversion gate does.
fn mutated_snapshot(json: String) -> String {
    let Ok(mutation) = std::env::var("GENTS_SNAPSHOT_MUTATION") else {
        return json;
    };
    let (key, field) = match mutation.split_once('.') {
        Some((key, field)) => (key, Some(field)),
        None => (mutation.as_str(), None),
    };
    let mut snapshot: serde_json::Value = serde_json::from_str(&json).expect("snapshot JSON");
    let cases = snapshot.get_mut(key).expect("mutated snapshot key");
    mutate_leaves(cases, field, field.is_none());
    snapshot.to_string()
}

fn mutate_leaves(value: &mut serde_json::Value, field: Option<&str>, selected: bool) {
    match value {
        serde_json::Value::Bool(flag) if selected => *flag = !*flag,
        serde_json::Value::Number(number) if selected => {
            *value = serde_json::Value::from(number.as_u64().unwrap_or(0) + 1);
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mutate_leaves(item, field, selected);
            }
        }
        serde_json::Value::Object(fields) => {
            for (name, item) in fields {
                mutate_leaves(item, field, selected || field == Some(name.as_str()));
            }
        }
        _ => {}
    }
}

fn load_lean_contract_snapshot() -> LeanContractSnapshot {
    let proofs_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../gents/proofs"));
    let stdout = gents_lean_contract::run_prepared_generator(
        proofs_dir,
        LEAN_CONTRACT_TARGET,
        LEAN_CONTRACT_FILE,
    )
    .unwrap_or_else(|error| panic!("failed to load Lean conformance contract JSON: {error:#}"));
    let json = gents_lean_contract::extract_contract_json(&stdout)
        .unwrap_or_else(|error| panic!("failed to load Lean conformance contract JSON: {error:#}"))
        .to_owned();
    let json = mutated_snapshot(json);

    serde_json::from_str(&json).unwrap_or_else(|error| {
        let excerpt = json_excerpt_at(&json, error.line(), error.column(), 512);
        panic!(
            "failed to parse Lean conformance contract JSON ({} bytes) at line {}, column {}: {error}\nnear error: {excerpt}",
            json.len(),
            error.line(),
            error.column(),
        )
    })
}

fn json_excerpt_at(json: &str, line: usize, column: usize, max_chars: usize) -> String {
    let source_line = json.lines().nth(line.saturating_sub(1)).unwrap_or_default();
    let error_char = column.saturating_sub(1);
    let start = error_char.saturating_sub(max_chars / 2);
    let excerpt = source_line
        .chars()
        .skip(start)
        .take(max_chars)
        .collect::<String>();
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if source_line.chars().count() > start + excerpt.chars().count() {
        "…"
    } else {
        ""
    };
    format!("{prefix}{excerpt}{suffix}")
}

pub fn lean_to_defradb_values<'a>(
    lean_file: &str,
    model: &'a str,
    namespace: &str,
) -> Vec<&'a str> {
    parse_lean_to_defradb_values(model, namespace)
        .unwrap_or_else(|error| panic!("{}", error.message(lean_file, namespace)))
}

pub(crate) fn parse_lean_to_defradb_values<'a>(
    model: &'a str,
    namespace: &str,
) -> Result<Vec<&'a str>, LeanVocabularyParseError<'a>> {
    let namespace_start = format!("namespace {namespace}");
    let namespace_end = format!("end {namespace}");
    let mut found_namespace = false;
    let mut found_to_defradb = false;
    let mut in_namespace = false;
    let mut in_to_defradb = false;
    let mut values = Vec::new();

    for (index, line) in model.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();

        if !in_namespace {
            if trimmed == namespace_start {
                found_namespace = true;
                in_namespace = true;
            }
            continue;
        }

        if trimmed == namespace_end {
            break;
        }

        if !in_to_defradb {
            if trimmed.starts_with("def toDefraDB") {
                found_to_defradb = true;
                in_to_defradb = true;
            }
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }

        if trimmed.starts_with('|') {
            values.push(parse_to_defradb_arm(trimmed, line_number)?);
            continue;
        }

        if values.is_empty() {
            return Err(LeanVocabularyParseError::MalformedArm {
                line_number,
                line: trimmed,
                reason: "expected a toDefraDB pattern arm starting with `| .`",
            });
        }

        break;
    }

    if !found_namespace {
        return Err(LeanVocabularyParseError::MissingNamespace);
    }
    if !found_to_defradb {
        return Err(LeanVocabularyParseError::MissingToDefraDB);
    }
    if values.is_empty() {
        return Err(LeanVocabularyParseError::EmptyToDefraDB);
    }

    Ok(values)
}

fn parse_to_defradb_arm<'a>(
    trimmed: &'a str,
    line_number: usize,
) -> Result<&'a str, LeanVocabularyParseError<'a>> {
    let Some(rest) = trimmed.strip_prefix("| .") else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line: trimmed,
            reason: "expected a toDefraDB pattern arm starting with `| .`",
        });
    };
    let Some((_constructor, value)) = rest.split_once("=>") else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line: trimmed,
            reason: "missing `=>`",
        });
    };

    parse_string_literal(value.trim(), line_number, trimmed)
}

fn parse_string_literal<'a>(
    value: &'a str,
    line_number: usize,
    line: &'a str,
) -> Result<&'a str, LeanVocabularyParseError<'a>> {
    let Some(after_opening_quote) = value.strip_prefix('"') else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "expected a string literal after `=>`",
        });
    };
    let Some(end_index) = after_opening_quote.find('"') else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "string literal is missing a closing quote",
        });
    };
    let literal = &after_opening_quote[..end_index];
    let trailing = after_opening_quote[end_index + 1..].trim();
    if !trailing.is_empty() && !trailing.starts_with("--") {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "expected only optional comment text after the string literal",
        });
    }

    Ok(literal)
}

fn values_missing_from<'a>(expected: &[&'a str], actual: &[&str]) -> Vec<&'a str> {
    expected
        .iter()
        .copied()
        .filter(|value| !actual.contains(value))
        .collect()
}

fn duplicate_values<'a>(values: &[&'a str]) -> Vec<&'a str> {
    let mut seen = Vec::new();
    let mut duplicates = Vec::new();
    for value in values {
        if seen.contains(value) {
            if !duplicates.contains(value) {
                duplicates.push(*value);
            }
        } else {
            seen.push(*value);
        }
    }
    duplicates
}

impl LeanVocabularyParseError<'_> {
    fn message(&self, lean_file: &str, namespace: &str) -> String {
        match self {
            Self::MissingNamespace => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: namespace block was not found"
            ),
            Self::MissingToDefraDB => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: def toDefraDB was not found in the namespace"
            ),
            Self::EmptyToDefraDB => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: def toDefraDB has no parsed string-valued arms"
            ),
            Self::MalformedArm {
                line_number,
                line,
                reason,
            } => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  line: {line_number}\n  reason: {reason}\n  source: {line}"
            ),
        }
    }
}
