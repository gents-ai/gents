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

#[derive(Debug, Clone, Copy)]
pub struct LeanContractVocabulary<'a> {
    pub domain: &'a str,
    pub rust_source: &'a str,
    pub rust_values: &'a [&'a str],
}

impl LeanPluginJobAdoptionCase {
    pub fn name(&self) -> &str {
        match self {
            Self::Step0 { name, .. }
            | Self::HostPin { name, .. }
            | Self::Handle { name, .. }
            | Self::Address { name, .. } => name,
        }
    }
}

impl LeanPluginJobControlCase {
    pub fn name(&self) -> &str {
        match self {
            Self::Kill { name, .. }
            | Self::Resume { name, .. }
            | Self::ResultRead { name, .. }
            | Self::ResultOffer { name, .. }
            | Self::StepFailure { name, .. } => name,
        }
    }
}

impl LeanPluginJobAuthorityCase {
    pub fn name(&self) -> &str {
        match self {
            Self::Provenance { name, .. }
            | Self::Install { name, .. }
            | Self::Installer { name, .. }
            | Self::Origin { name, .. }
            | Self::PackJobBlock { name, .. }
            | Self::PolicyValid { name, .. }
            | Self::ConsentedDynamic { name, .. }
            | Self::WireTargetsFrom { name, .. }
            | Self::PublishedRead { name, .. } => name,
        }
    }
}

/// `plugin_resource_host_import_cases`: whether the `gents` imports are
/// linked for a plugin and what `run_script` answers.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginHostImportCase {
    pub declared: bool,
    pub call_has_script: bool,
    pub linked: bool,
    pub answer: Option<String>,
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

mod background_transcript;
mod canonical_execution;
mod canonical_output;
pub mod canonical_presentation;
mod compaction_canonical_projection;
mod compaction_projection_join;
pub mod consumer_scan;
pub mod family;
mod prompt_assembly;
mod request_execution_lease;
mod title;

/// Family types generated from `families.schema.json`, which Lean emits.
pub mod families {
    include!(concat!(env!("OUT_DIR"), "/families.rs"));
}

pub use canonical_execution::*;
pub use canonical_output::*;
pub use canonical_presentation::native_adapter::native_presentation;
pub use canonical_presentation::*;
pub use compaction_canonical_projection::*;
pub use compaction_projection_join::*;
pub use prompt_assembly::*;
pub use request_execution_lease::*;
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

pub fn lean_plugin_host_import_cases() -> &'static [LeanPluginHostImportCase] {
    static CASES: OnceLock<Vec<LeanPluginHostImportCase>> = OnceLock::new();
    CASES.get_or_init(|| {
        lean_plugin_resource_group(
            "plugin_resource_host_import_cases",
            &lean_contract_snapshot().plugin_resource_host_import_cases,
        )
    })
}

pub fn lean_vocabulary_contract(domain: &str) -> &'static LeanVocabularyContract {
    lean_contract_snapshot()
        .vocabularies
        .iter()
        .find(|contract| contract.domain == domain)
        .unwrap_or_else(|| panic!("Lean vocabulary contract {domain:?} was not emitted"))
}

pub fn lean_state_machine_contract(domain: &str) -> &'static LeanStateMachineContract {
    lean_contract_snapshot()
        .state_machines
        .iter()
        .find(|contract| contract.domain == domain)
        .unwrap_or_else(|| panic!("Lean state-machine contract {domain:?} was not emitted"))
}

pub fn lean_runtime_reconcile_case(name: &str) -> &'static LeanRuntimeReconcileCase {
    lean_contract_snapshot()
        .runtime_reconcile_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean runtime-reconcile case {name:?} was not emitted"))
}

pub fn lean_session_recovery_case(name: &str) -> &'static LeanSessionRecoveryCase {
    lean_contract_snapshot()
        .session_recovery_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean session-recovery case {name:?} was not emitted"))
}

pub fn lean_client_shell_case(name: &str) -> &'static LeanClientShellCase {
    lean_contract_snapshot()
        .frontend_client_shell_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean ClientShell case {name:?} was not emitted"))
}

pub fn lean_inference_slot_accounting_case(name: &str) -> &'static LeanInferenceSlotAccountingCase {
    lean_contract_snapshot()
        .inference_slot_accounting_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean inference slot-accounting case {name:?} was not emitted"))
}

pub fn lean_fleet_slot_accounting_case(name: &str) -> &'static LeanFleetSlotAccountingCase {
    lean_contract_snapshot()
        .fleet_slot_accounting_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean fleet slot-accounting case {name:?} was not emitted"))
}

pub fn lean_queue_deadline_case(name: &str) -> &'static LeanQueueDeadlineConformanceCase {
    lean_contract_snapshot()
        .queue_deadline_conformance_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean queue/deadline case {name:?} was not emitted"))
}

pub fn lean_recovery_sweep_case(name: &str) -> &'static LeanRecoverySweepCase {
    lean_contract_snapshot()
        .recovery_sweep_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean recovery sweep case {name:?} was not emitted"))
}

pub fn lean_r4c_background_work_case(witness: &str) -> &'static LeanR4cBackgroundWorkCase {
    lean_contract_snapshot()
        .r4c_background_work_cases
        .iter()
        .find(|case| case.witness() == witness)
        .unwrap_or_else(|| panic!("Lean R4c background-work witness {witness:?} was not emitted"))
}

pub fn lean_r6_backgrounding_case(name: &str) -> &'static LeanR6BackgroundingCase {
    lean_contract_snapshot()
        .r6_backgrounding_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean R6 backgrounding case {name:?} was not emitted"))
}

pub fn lean_composed_invariant_witness(
    theorem_name: &str,
) -> &'static LeanComposedInvariantWitness {
    lean_contract_snapshot()
        .composed_invariant_witnesses
        .iter()
        .find(|witness| witness.theorem_name == theorem_name)
        .unwrap_or_else(|| {
            panic!("Lean composed invariant witness {theorem_name:?} was not emitted")
        })
}

pub fn lean_composed_invariant_witness_by_scenario(
    scenario: &str,
) -> &'static LeanComposedInvariantWitness {
    lean_contract_snapshot()
        .composed_invariant_witnesses
        .iter()
        .find(|witness| witness.scenario == scenario)
        .unwrap_or_else(|| {
            panic!("Lean composed invariant witness scenario {scenario:?} was not emitted")
        })
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

impl LeanQueueManagementState {
    pub fn image(&self, native: impl Fn(u64) -> String) -> QueueImage {
        let ids = |entries: &[LeanFoldQueueEntry]| {
            entries
                .iter()
                .map(|entry| native(entry.request_id))
                .collect()
        };
        QueueImage {
            active: self.active.map(&native),
            folding: ids(&self.folding),
            pending: ids(&self.pending),
            terminal: self.terminal.iter().copied().map(&native).collect(),
        }
    }
}

pub fn lean_compaction_reducer_case(name: &str) -> &'static LeanCompactionReducerCase {
    lean_contract_snapshot()
        .compaction_reducer_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean compaction-reducer case {name:?} was not emitted"))
}

pub fn lean_command_policy_case(name: &str) -> &'static LeanCommandPolicyCase {
    lean_contract_snapshot()
        .command_policy_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command policy case {name:?} was not emitted"))
}

pub fn lean_command_env_case(name: &str) -> &'static LeanCommandEnvCase {
    lean_contract_snapshot()
        .command_env_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command env case {name:?} was not emitted"))
}

pub fn lean_vocabulary_values(domain: &str) -> Vec<&'static str> {
    lean_vocabulary_contract(domain)
        .values
        .iter()
        .map(String::as_str)
        .collect()
}

pub fn assert_lean_contract_vocabulary_matches(spec: LeanContractVocabulary<'_>) {
    let lean_values = lean_vocabulary_values(spec.domain);
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
        "Rust/Lean vocabulary contract mismatch\n  Lean contract domain: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.domain,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

pub fn assert_lean_contract_vocabulary_set_matches(spec: LeanContractVocabulary<'_>) {
    let lean_values = lean_vocabulary_values(spec.domain);
    let missing_from_lean = values_missing_from(spec.rust_values, &lean_values);
    let extra_in_lean = values_missing_from(&lean_values, spec.rust_values);
    let duplicate_rust_values = duplicate_values(spec.rust_values);
    let duplicate_lean_values = duplicate_values(&lean_values);

    assert!(
        missing_from_lean.is_empty()
            && extra_in_lean.is_empty()
            && duplicate_rust_values.is_empty()
            && duplicate_lean_values.is_empty(),
        "Rust/Lean vocabulary contract set mismatch\n  Lean contract domain: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.domain,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

pub fn assert_lean_transition_is_legal(domain: &str, from: &str, to: &str) {
    let machine = lean_state_machine_contract(domain);
    assert!(
        machine
            .legal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} does not allow transition {from:?} -> {to:?}\n  legal transitions: {:?}",
        machine.legal_transitions
    );
    assert!(
        !machine
            .illegal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} marks transition {from:?} -> {to:?} as both legal and illegal"
    );
}

pub fn assert_lean_transition_is_illegal(domain: &str, from: &str, to: &str) {
    let machine = lean_state_machine_contract(domain);
    assert!(
        machine
            .illegal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} does not mark transition {from:?} -> {to:?} illegal\n  illegal transitions: {:?}",
        machine.illegal_transitions
    );
    assert!(
        !machine
            .legal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} marks transition {from:?} -> {to:?} as both legal and illegal"
    );
}

pub fn assert_state_machine_contract_is_complete(domain: &str) {
    let machine = lean_state_machine_contract(domain);
    let duplicate_states = duplicate_string_values(&machine.states);
    let duplicate_actions = duplicate_string_values(&machine.actions);
    let duplicate_legal_pairs = duplicate_transition_pairs(&machine.legal_transitions);
    let duplicate_illegal_pairs = duplicate_transition_pairs(&machine.illegal_transitions);
    let expected_pairs = machine.state_count * machine.state_count;
    let actual_pairs = machine.legal_transitions.len() + machine.illegal_transitions.len();

    assert!(
        machine.state_count == machine.states.len()
            && duplicate_states.is_empty()
            && duplicate_actions.is_empty()
            && duplicate_legal_pairs.is_empty()
            && duplicate_illegal_pairs.is_empty()
            && actual_pairs == expected_pairs
            && machine
                .legal_transitions
                .iter()
                .all(|pair| !machine.illegal_transitions.contains(pair))
            && machine.legal_transitions.iter().all(
                |pair| machine.states.contains(&pair.from) && machine.states.contains(&pair.to)
            )
            && machine.illegal_transitions.iter().all(
                |pair| machine.states.contains(&pair.from) && machine.states.contains(&pair.to)
            ),
        "Lean state-machine contract {domain:?} is incomplete or malformed\n  state_count: {}\n  states: {:?}\n  actions: {:?}\n  legal transitions: {:?}\n  illegal transitions: {:?}\n  duplicate states: {:?}\n  duplicate actions: {:?}\n  duplicate legal pairs: {:?}\n  duplicate illegal pairs: {:?}\n  expected pair partition size: {}\n  actual pair partition size: {}",
        machine.state_count,
        machine.states,
        machine.actions,
        machine.legal_transitions,
        machine.illegal_transitions,
        duplicate_states,
        duplicate_actions,
        duplicate_legal_pairs,
        duplicate_illegal_pairs,
        expected_pairs,
        actual_pairs
    );
}

pub fn assert_lifecycle_transition_cases_partition(
    domain: &str,
    states: &[&str],
    cases: &[LeanLifecycleTransitionCase],
) {
    let expected_pairs = states
        .iter()
        .flat_map(|from| {
            states
                .iter()
                .map(move |to| ((*from).to_string(), (*to).to_string()))
        })
        .collect::<BTreeSet<_>>();
    let mut actual_pairs = BTreeSet::new();
    let mut invalid_cases = Vec::new();

    for case in cases {
        if case.domain != domain {
            invalid_cases.push(format!("{} has wrong domain {:?}", case.name, case.domain));
        }
        if !matches!(
            case.classification.as_str(),
            "legal" | "illegal" | "recoveryReachable"
        ) {
            invalid_cases.push(format!(
                "{} has invalid classification {:?}",
                case.name, case.classification
            ));
        }
        if case.classification == "legal" && case.action.is_none() {
            invalid_cases.push(format!("{} legal case missing action", case.name));
        }
        if case.classification != "legal" && case.action.is_some() {
            invalid_cases.push(format!("{} non-legal case has action", case.name));
        }
        let requires_boundary = case.classification == "recoveryReachable";
        if requires_boundary && case.boundary.is_none() {
            invalid_cases.push(format!(
                "{} {} case missing boundary",
                case.name, case.classification
            ));
        }
        if !requires_boundary && case.boundary.is_some() {
            invalid_cases.push(format!("{} reachable case has boundary", case.name));
        }
        if !expected_pairs.contains(&(case.from.clone(), case.to.clone())) {
            invalid_cases.push(format!(
                "{} has pair outside {domain} vocabulary: {:?} -> {:?}",
                case.name, case.from, case.to
            ));
        }
        if !actual_pairs.insert((case.from.clone(), case.to.clone())) {
            invalid_cases.push(format!(
                "{} duplicates pair {:?} -> {:?}",
                case.name, case.from, case.to
            ));
        }
    }

    let missing = expected_pairs
        .difference(&actual_pairs)
        .cloned()
        .collect::<Vec<_>>();
    let extra = actual_pairs
        .difference(&expected_pairs)
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        invalid_cases.is_empty() && missing.is_empty() && extra.is_empty(),
        "Lean lifecycle transition cases for {domain:?} do not form a state^2 partition\n  invalid cases: {:?}\n  missing pairs: {:?}\n  extra pairs: {:?}\n  cases: {:?}",
        invalid_cases,
        missing,
        extra,
        cases
    );
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

fn duplicate_string_values(values: &[String]) -> Vec<String> {
    let refs = values.iter().map(String::as_str).collect::<Vec<_>>();
    duplicate_values(&refs)
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn duplicate_transition_pairs(values: &[LeanTransitionPair]) -> Vec<LeanTransitionPair> {
    let mut seen = Vec::new();
    let mut duplicates = Vec::new();
    for value in values {
        if seen.contains(value) {
            if !duplicates.contains(value) {
                duplicates.push(value.clone());
            }
        } else {
            seen.push(value.clone());
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
