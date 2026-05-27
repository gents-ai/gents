#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::Deserialize;

pub(crate) type LeanFeatureMatrix = BTreeMap<String, BTreeMap<String, LeanFeatureMatrixCell>>;

#[derive(Debug, Clone, Copy)]
pub(crate) struct LeanVocabulary<'a> {
    pub(crate) lean_file: &'a str,
    pub(crate) model: &'a str,
    pub(crate) namespace: &'a str,
    pub(crate) rust_source: &'a str,
    pub(crate) rust_values: &'a [&'a str],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LeanContractVocabulary<'a> {
    pub(crate) domain: &'a str,
    pub(crate) rust_source: &'a str,
    pub(crate) rust_values: &'a [&'a str],
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanContractSnapshot {
    pub(crate) generated_by: String,
    pub(crate) vocabularies: Vec<LeanVocabularyContract>,
    pub(crate) state_machines: Vec<LeanStateMachineContract>,
    pub(crate) request_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub(crate) process_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub(crate) trigger_dispatch_case_count: usize,
    pub(crate) trigger_dispatch_cases: Vec<LeanTriggerDispatchCase>,
    pub(crate) frontend_client_shell_case_count: usize,
    pub(crate) frontend_client_shell_cases: Vec<LeanClientShellCase>,
    pub(crate) desktop_client_shell_case_count: usize,
    pub(crate) desktop_client_shell_cases: Vec<LeanClientShellCase>,
    pub(crate) request_lifecycle_operator_ui_cases: Vec<LeanClientShellCase>,
    pub(crate) runtime_reconcile_cases: Vec<LeanRuntimeReconcileCase>,
    pub(crate) apply_reconcile_cases: Vec<LeanApplyReconcileCase>,
    pub(crate) session_recovery_cases: Vec<LeanSessionRecoveryCase>,
    pub(crate) inference_slot_accounting_cases: Vec<LeanInferenceSlotAccountingCase>,
    pub(crate) fleet_slot_accounting_cases: Vec<LeanFleetSlotAccountingCase>,
    pub(crate) persistence_failure_policy_cases: Vec<LeanPersistenceFailurePolicyCase>,
    pub(crate) storage_observation_runtime_cases: Vec<LeanStorageObservationRuntimeCase>,
    pub(crate) backend_health_admission_cases: Vec<LeanBackendHealthAdmissionCase>,
    pub(crate) native_filesystem_boundary_cases: Vec<LeanNativeFilesystemBoundaryCase>,
    pub(crate) managed_exec_liveness_cases: Vec<LeanManagedExecLivenessCase>,
    pub(crate) tool_preflight_cases: Vec<LeanToolPreflightCase>,
    pub(crate) tool_retry_cases: Vec<LeanToolRetryCase>,
    pub(crate) mcp_health_cases: Vec<LeanMcpHealthCase>,
    pub(crate) boundaries: Vec<LeanBoundary>,
    pub(crate) deviations: Vec<LeanDeviation>,
    pub(crate) command_policy_cases: Vec<LeanCommandPolicyCase>,
    pub(crate) command_sandbox_cases: Vec<LeanCommandSandboxCase>,
    pub(crate) command_env_cases: Vec<LeanCommandEnvCase>,
    pub(crate) live_overlay_cases: Vec<LeanLiveOverlayCase>,
    pub(crate) queue_deadline_conformance_cases: Vec<LeanQueueDeadlineConformanceCase>,
    pub(crate) recovery_sweep_cases: Vec<LeanRecoverySweepCase>,
    #[serde(default)]
    pub(crate) r4c_background_work_cases: Vec<LeanR4cBackgroundWorkCase>,
    #[serde(default)]
    pub(crate) codex_shim_projection_cases: Vec<LeanCodexShimProjectionCase>,
    #[serde(default)]
    pub(crate) codex_shim_steering_cases: Vec<LeanCodexShimSteeringCase>,
    pub(crate) r6_backgrounding_cases: Vec<LeanR6BackgroundingCase>,
    #[serde(default)]
    pub(crate) r5_cross_deployment_cases: Vec<LeanR5CrossDeploymentCase>,
    pub(crate) r6_background_theorem_witnesses: Vec<LeanBackgroundTheoremWitness>,
    pub(crate) transcript_conformance_cases: Vec<LeanTranscriptCase>,
    pub(crate) streaming_response_cases: Vec<LeanResponseTransitionCase>,
    pub(crate) compaction_reducer_cases: Vec<LeanCompactionReducerCase>,
    pub(crate) follow_up_hooks: Vec<String>,
    pub(crate) coverage_ledger: Vec<LeanCoverageEntry>,
    pub(crate) feature_surface_requirements: Vec<LeanFeatureSurfaceRequirement>,
    pub(crate) feature_matrix: LeanFeatureMatrix,
    pub(crate) identity_structural_cases: Vec<LeanIdentityStructuralCase>,
    pub(crate) identity_permission_cases: Vec<LeanIdentityPermissionCase>,
    pub(crate) identity_contracts: Vec<LeanIdentityContract>,
    #[serde(default)]
    pub(crate) event_delivery_transition_case_count: usize,
    #[serde(default)]
    pub(crate) event_delivery_transition_cases: Vec<LeanEventDeliveryTransitionCase>,
    #[serde(default)]
    pub(crate) event_delivery_source_instances: Vec<LeanEventDeliverySourceInstance>,
    #[serde(default)]
    pub(crate) event_delivery_convergence_traces: Vec<LeanEventDeliveryConvergenceTrace>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanVocabularyContract {
    pub(crate) domain: String,
    pub(crate) values: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanStateMachineContract {
    pub(crate) domain: String,
    pub(crate) states: Vec<String>,
    pub(crate) state_count: usize,
    pub(crate) terminal_states: Vec<String>,
    pub(crate) nonterminal_states: Vec<String>,
    pub(crate) actions: Vec<String>,
    pub(crate) legal_transitions: Vec<LeanTransitionPair>,
    pub(crate) illegal_transitions: Vec<LeanTransitionPair>,
    /// Named transition rows emitted by `Conformance.Contracts.NamedTransition`.
    /// Defaults to empty so existing call sites (Bucket 0/Bucket 1) don't need
    /// to deserialize this field on machines that don't emit any.
    #[serde(default)]
    pub(crate) named_transitions: Vec<LeanNamedTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanNamedTransition {
    pub(crate) name: String,
    pub(crate) from: String,
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) requires_native: bool,
    #[serde(default)]
    pub(crate) requires_child: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCoverageEntry {
    pub(crate) category: String,
    pub(crate) domain: String,
    pub(crate) consumer: String,
    pub(crate) accepted_boundary: String,
    pub(crate) accepted_follow_up: String,
    #[serde(default)]
    pub(crate) feature: String,
    #[serde(default)]
    pub(crate) surfaces: Vec<LeanSurface>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LeanSurface {
    AgentFacing,
    OperatorCli,
    OperatorUi,
    Api,
    RuntimeInternal,
}

impl LeanSurface {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AgentFacing => "agentFacing",
            Self::OperatorCli => "operatorCli",
            Self::OperatorUi => "operatorUi",
            Self::Api => "api",
            Self::RuntimeInternal => "runtimeInternal",
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanFeatureSurfaceRequirement {
    pub(crate) feature: String,
    pub(crate) required: Vec<LeanSurface>,
    pub(crate) deferred: Vec<LeanFeatureSurfaceDeferral>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanFeatureSurfaceDeferral {
    pub(crate) surface: LeanSurface,
    pub(crate) note: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanFeatureMatrixCell {
    pub(crate) coverage_strength: String,
    pub(crate) row_count: usize,
    pub(crate) pending_follow_ups: usize,
    pub(crate) deferred_note: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanBoundary {
    pub(crate) id: String,
    pub(crate) domain: String,
    pub(crate) subject: String,
    pub(crate) statement: String,
    pub(crate) accepted_failure_mode: Option<String>,
    pub(crate) accepted_follow_up: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanDeviation {
    pub(crate) id: String,
    pub(crate) domain: String,
    pub(crate) subject: String,
    pub(crate) statement: String,
    pub(crate) accepted_failure_mode: Option<String>,
    pub(crate) accepted_follow_up: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanTransitionPair {
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanLifecycleTransitionCase {
    pub(crate) name: String,
    pub(crate) domain: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) classification: String,
    pub(crate) action: Option<String>,
    pub(crate) boundary: Option<String>,
}

#[path = "lean_vocab_test/background_transcript.rs"]
mod background_transcript;
#[path = "lean_vocab_test/client_session.rs"]
mod client_session;
#[path = "lean_vocab_test/codex_shim.rs"]
mod codex_shim;
#[path = "lean_vocab_test/command_identity_queue.rs"]
mod command_identity_queue;
#[path = "lean_vocab_test/event_delivery.rs"]
mod event_delivery;
#[path = "lean_vocab_test/slot_persistence_health.rs"]
mod slot_persistence_health;
#[path = "lean_vocab_test/triggers_runtime_apply.rs"]
mod triggers_runtime_apply;

pub(crate) use background_transcript::*;
pub(crate) use client_session::*;
pub(crate) use codex_shim::*;
pub(crate) use command_identity_queue::*;
pub(crate) use event_delivery::*;
pub(crate) use slot_persistence_health::*;
pub(crate) use triggers_runtime_apply::*;

#[derive(Debug, PartialEq, Eq)]
enum LeanVocabularyParseError<'a> {
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

pub(crate) fn lean_contract_snapshot() -> &'static LeanContractSnapshot {
    LEAN_CONTRACT_SNAPSHOT.get_or_init(load_lean_contract_snapshot)
}

pub(crate) fn lean_vocabulary_contract(domain: &str) -> &'static LeanVocabularyContract {
    lean_contract_snapshot()
        .vocabularies
        .iter()
        .find(|contract| contract.domain == domain)
        .unwrap_or_else(|| panic!("Lean vocabulary contract {domain:?} was not emitted"))
}

pub(crate) fn lean_state_machine_contract(domain: &str) -> &'static LeanStateMachineContract {
    lean_contract_snapshot()
        .state_machines
        .iter()
        .find(|contract| contract.domain == domain)
        .unwrap_or_else(|| panic!("Lean state-machine contract {domain:?} was not emitted"))
}

pub(crate) fn lean_feature_surface_requirements() -> &'static [LeanFeatureSurfaceRequirement] {
    &lean_contract_snapshot().feature_surface_requirements
}

pub(crate) fn lean_feature_matrix() -> &'static LeanFeatureMatrix {
    &lean_contract_snapshot().feature_matrix
}

pub(crate) fn lean_request_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().request_transition_cases
}

pub(crate) fn lean_process_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().process_transition_cases
}

pub(crate) fn lean_runtime_reconcile_case(name: &str) -> &'static LeanRuntimeReconcileCase {
    lean_contract_snapshot()
        .runtime_reconcile_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean runtime-reconcile case {name:?} was not emitted"))
}

pub(crate) fn lean_apply_reconcile_cases() -> &'static [LeanApplyReconcileCase] {
    &lean_contract_snapshot().apply_reconcile_cases
}

pub(crate) fn lean_apply_reconcile_case(name: &str) -> &'static LeanApplyReconcileCase {
    lean_contract_snapshot()
        .apply_reconcile_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean apply-reconcile case {name:?} was not emitted"))
}

pub(crate) fn lean_session_recovery_case(name: &str) -> &'static LeanSessionRecoveryCase {
    lean_contract_snapshot()
        .session_recovery_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean session-recovery case {name:?} was not emitted"))
}

pub(crate) fn lean_client_shell_case(name: &str) -> &'static LeanClientShellCase {
    lean_contract_snapshot()
        .frontend_client_shell_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean ClientShell case {name:?} was not emitted"))
}

pub(crate) fn lean_desktop_client_shell_cases() -> &'static [LeanClientShellCase] {
    &lean_contract_snapshot().desktop_client_shell_cases
}

pub(crate) fn lean_request_lifecycle_operator_ui_cases() -> &'static [LeanClientShellCase] {
    &lean_contract_snapshot().request_lifecycle_operator_ui_cases
}

pub(crate) fn lean_inference_slot_accounting_cases() -> &'static [LeanInferenceSlotAccountingCase] {
    &lean_contract_snapshot().inference_slot_accounting_cases
}

pub(crate) fn lean_inference_slot_accounting_case(
    name: &str,
) -> &'static LeanInferenceSlotAccountingCase {
    lean_contract_snapshot()
        .inference_slot_accounting_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean inference slot-accounting case {name:?} was not emitted"))
}

pub(crate) fn lean_fleet_slot_accounting_case(name: &str) -> &'static LeanFleetSlotAccountingCase {
    lean_contract_snapshot()
        .fleet_slot_accounting_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean fleet slot-accounting case {name:?} was not emitted"))
}

pub(crate) fn lean_persistence_failure_policy_cases() -> &'static [LeanPersistenceFailurePolicyCase]
{
    &lean_contract_snapshot().persistence_failure_policy_cases
}

pub(crate) fn lean_storage_observation_runtime_cases(
) -> &'static [LeanStorageObservationRuntimeCase] {
    &lean_contract_snapshot().storage_observation_runtime_cases
}

pub(crate) fn lean_backend_health_admission_cases() -> &'static [LeanBackendHealthAdmissionCase] {
    &lean_contract_snapshot().backend_health_admission_cases
}

pub(crate) fn lean_native_filesystem_boundary_cases() -> &'static [LeanNativeFilesystemBoundaryCase]
{
    &lean_contract_snapshot().native_filesystem_boundary_cases
}

pub(crate) fn lean_managed_exec_liveness_cases() -> &'static [LeanManagedExecLivenessCase] {
    &lean_contract_snapshot().managed_exec_liveness_cases
}

pub(crate) fn lean_tool_preflight_cases() -> &'static [LeanToolPreflightCase] {
    &lean_contract_snapshot().tool_preflight_cases
}

pub(crate) fn lean_tool_preflight_case(name: &str) -> &'static LeanToolPreflightCase {
    lean_contract_snapshot()
        .tool_preflight_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean tool preflight case {name:?} was not emitted"))
}

pub(crate) fn lean_tool_retry_cases() -> &'static [LeanToolRetryCase] {
    &lean_contract_snapshot().tool_retry_cases
}

pub(crate) fn lean_queue_deadline_cases() -> &'static [LeanQueueDeadlineConformanceCase] {
    &lean_contract_snapshot().queue_deadline_conformance_cases
}

pub(crate) fn lean_queue_deadline_case(name: &str) -> &'static LeanQueueDeadlineConformanceCase {
    lean_contract_snapshot()
        .queue_deadline_conformance_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean queue/deadline case {name:?} was not emitted"))
}

pub(crate) fn lean_recovery_sweep_cases() -> &'static [LeanRecoverySweepCase] {
    &lean_contract_snapshot().recovery_sweep_cases
}

pub(crate) fn lean_recovery_sweep_case(name: &str) -> &'static LeanRecoverySweepCase {
    lean_contract_snapshot()
        .recovery_sweep_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean recovery sweep case {name:?} was not emitted"))
}

pub(crate) fn lean_r4c_background_work_cases() -> &'static [LeanR4cBackgroundWorkCase] {
    &lean_contract_snapshot().r4c_background_work_cases
}

pub(crate) fn lean_r4c_background_work_case(witness: &str) -> &'static LeanR4cBackgroundWorkCase {
    lean_contract_snapshot()
        .r4c_background_work_cases
        .iter()
        .find(|case| case.witness() == witness)
        .unwrap_or_else(|| panic!("Lean R4c background-work witness {witness:?} was not emitted"))
}

pub(crate) fn lean_codex_shim_projection_cases() -> &'static [LeanCodexShimProjectionCase] {
    &lean_contract_snapshot().codex_shim_projection_cases
}

pub(crate) fn lean_codex_shim_projection_case(
    witness: &str,
) -> &'static LeanCodexShimProjectionCase {
    lean_contract_snapshot()
        .codex_shim_projection_cases
        .iter()
        .find(|case| case.witness == witness)
        .unwrap_or_else(|| panic!("Lean Codex shim projection witness {witness:?} was not emitted"))
}

pub(crate) fn lean_codex_shim_steering_cases() -> &'static [LeanCodexShimSteeringCase] {
    &lean_contract_snapshot().codex_shim_steering_cases
}

pub(crate) fn lean_codex_shim_steering_case(witness: &str) -> &'static LeanCodexShimSteeringCase {
    lean_contract_snapshot()
        .codex_shim_steering_cases
        .iter()
        .find(|case| case.witness == witness)
        .unwrap_or_else(|| panic!("Lean Codex shim steering witness {witness:?} was not emitted"))
}

pub(crate) fn lean_r6_backgrounding_cases() -> &'static [LeanR6BackgroundingCase] {
    &lean_contract_snapshot().r6_backgrounding_cases
}

pub(crate) fn lean_r6_backgrounding_case(name: &str) -> &'static LeanR6BackgroundingCase {
    lean_contract_snapshot()
        .r6_backgrounding_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean R6 backgrounding case {name:?} was not emitted"))
}

pub(crate) fn lean_r5_cross_deployment_cases() -> &'static [LeanR5CrossDeploymentCase] {
    &lean_contract_snapshot().r5_cross_deployment_cases
}

pub(crate) fn lean_r6_background_theorem_witnesses() -> &'static [LeanBackgroundTheoremWitness] {
    &lean_contract_snapshot().r6_background_theorem_witnesses
}

pub(crate) fn lean_r6_background_theorem_witness(
    theorem_name: &str,
) -> &'static LeanBackgroundTheoremWitness {
    lean_contract_snapshot()
        .r6_background_theorem_witnesses
        .iter()
        .find(|witness| witness.theorem_name == theorem_name)
        .unwrap_or_else(|| {
            panic!("Lean R6 background theorem witness {theorem_name:?} was not emitted")
        })
}

pub(crate) fn lean_transcript_cases() -> &'static [LeanTranscriptCase] {
    &lean_contract_snapshot().transcript_conformance_cases
}

pub(crate) fn lean_transcript_case(name: &str) -> &'static LeanTranscriptCase {
    lean_contract_snapshot()
        .transcript_conformance_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean transcript case {name:?} was not emitted"))
}

pub(crate) fn lean_response_transition_cases() -> &'static [LeanResponseTransitionCase] {
    &lean_contract_snapshot().streaming_response_cases
}

pub(crate) fn lean_response_transition_case(name: &str) -> &'static LeanResponseTransitionCase {
    lean_contract_snapshot()
        .streaming_response_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean response-transition case {name:?} was not emitted"))
}

pub(crate) fn lean_compaction_reducer_cases() -> &'static [LeanCompactionReducerCase] {
    &lean_contract_snapshot().compaction_reducer_cases
}

pub(crate) fn lean_compaction_reducer_case(name: &str) -> &'static LeanCompactionReducerCase {
    lean_contract_snapshot()
        .compaction_reducer_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean compaction-reducer case {name:?} was not emitted"))
}

pub(crate) fn lean_event_delivery_transition_cases() -> &'static [LeanEventDeliveryTransitionCase] {
    &lean_contract_snapshot().event_delivery_transition_cases
}

pub(crate) fn lean_event_delivery_source_instances() -> &'static [LeanEventDeliverySourceInstance] {
    &lean_contract_snapshot().event_delivery_source_instances
}

pub(crate) fn lean_event_delivery_convergence_traces(
) -> &'static [LeanEventDeliveryConvergenceTrace] {
    &lean_contract_snapshot().event_delivery_convergence_traces
}

pub(crate) fn lean_tool_retry_case(name: &str) -> &'static LeanToolRetryCase {
    lean_contract_snapshot()
        .tool_retry_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean tool retry case {name:?} was not emitted"))
}

pub(crate) fn lean_mcp_health_cases() -> &'static [LeanMcpHealthCase] {
    &lean_contract_snapshot().mcp_health_cases
}

pub(crate) fn lean_command_policy_cases() -> &'static [LeanCommandPolicyCase] {
    &lean_contract_snapshot().command_policy_cases
}

pub(crate) fn lean_command_policy_case(name: &str) -> &'static LeanCommandPolicyCase {
    lean_contract_snapshot()
        .command_policy_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command policy case {name:?} was not emitted"))
}

pub(crate) fn lean_command_sandbox_cases() -> &'static [LeanCommandSandboxCase] {
    &lean_contract_snapshot().command_sandbox_cases
}

pub(crate) fn lean_command_sandbox_case(name: &str) -> &'static LeanCommandSandboxCase {
    lean_contract_snapshot()
        .command_sandbox_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command sandbox case {name:?} was not emitted"))
}

pub(crate) fn lean_command_env_cases() -> &'static [LeanCommandEnvCase] {
    &lean_contract_snapshot().command_env_cases
}

pub(crate) fn lean_command_env_case(name: &str) -> &'static LeanCommandEnvCase {
    lean_contract_snapshot()
        .command_env_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command env case {name:?} was not emitted"))
}

pub(crate) fn lean_identity_structural_cases() -> &'static [LeanIdentityStructuralCase] {
    &lean_contract_snapshot().identity_structural_cases
}

pub(crate) fn lean_identity_permission_cases() -> &'static [LeanIdentityPermissionCase] {
    &lean_contract_snapshot().identity_permission_cases
}

pub(crate) fn lean_identity_contracts() -> &'static [LeanIdentityContract] {
    &lean_contract_snapshot().identity_contracts
}

pub(crate) fn lean_live_overlay_cases() -> &'static [LeanLiveOverlayCase] {
    &lean_contract_snapshot().live_overlay_cases
}

pub(crate) fn lean_vocabulary_values(domain: &str) -> Vec<&'static str> {
    lean_vocabulary_contract(domain)
        .values
        .iter()
        .map(String::as_str)
        .collect()
}

pub(crate) fn lean_trigger_dispatch_cases() -> &'static [LeanTriggerDispatchCase] {
    &lean_contract_snapshot().trigger_dispatch_cases
}

pub(crate) fn lean_trigger_dispatch_case_count() -> usize {
    lean_contract_snapshot().trigger_dispatch_case_count
}

pub(crate) fn assert_lean_contract_vocabulary_matches(spec: LeanContractVocabulary<'_>) {
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

pub(crate) fn assert_lean_contract_vocabulary_set_matches(spec: LeanContractVocabulary<'_>) {
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

pub(crate) fn assert_lean_transition_is_legal(domain: &str, from: &str, to: &str) {
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

pub(crate) fn assert_lean_transition_is_illegal(domain: &str, from: &str, to: &str) {
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

pub(crate) fn assert_state_machine_contract_is_complete(domain: &str) {
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
            && machine
                .legal_transitions
                .iter()
                .all(|pair| machine.states.contains(&pair.from) && machine.states.contains(&pair.to))
            && machine
                .illegal_transitions
                .iter()
                .all(|pair| machine.states.contains(&pair.from) && machine.states.contains(&pair.to)),
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

pub(crate) fn assert_lifecycle_transition_cases_partition(
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
            "legal" | "illegal" | "productUnreachable"
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
        if case.classification == "productUnreachable" && case.boundary.is_none() {
            invalid_cases.push(format!(
                "{} product-unreachable case missing boundary",
                case.name
            ));
        }
        if case.classification != "productUnreachable" && case.boundary.is_some() {
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

pub(crate) fn assert_lean_to_defradb_vocabulary_matches(spec: LeanVocabulary<'_>) {
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

fn load_lean_contract_snapshot() -> LeanContractSnapshot {
    defra_agent_lean_contract::load_contract_snapshot().unwrap_or_else(|error| panic!("{error:#}"))
}

pub(crate) fn lean_to_defradb_values<'a>(
    lean_file: &str,
    model: &'a str,
    namespace: &str,
) -> Vec<&'a str> {
    parse_lean_to_defradb_values(model, namespace)
        .unwrap_or_else(|error| panic!("{}", error.message(lean_file, namespace)))
}

fn parse_lean_to_defradb_values<'a>(
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

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL_LEAN: &str = r#"
inductive Sample where
  | one
  | two

namespace Sample

def toDefraDB : Sample -> String
  | .one => "one"
  | .two => "two"

def fromDefraDB? : String -> Option Sample
  | "one" => some .one
  | "two" => some .two
  | _ => none

end Sample
"#;

    #[test]
    fn parses_normal_to_defradb_values() {
        assert_eq!(
            parse_lean_to_defradb_values(NORMAL_LEAN, "Sample").unwrap(),
            vec!["one", "two"]
        );
    }

    #[test]
    fn reports_missing_namespace() {
        let error = parse_lean_to_defradb_values(NORMAL_LEAN, "Missing").unwrap_err();
        assert_eq!(error, LeanVocabularyParseError::MissingNamespace);
    }

    #[test]
    fn reports_missing_to_defradb() {
        let lean = r#"
namespace Sample

def unrelated : String := "value"

end Sample
"#;

        let error = parse_lean_to_defradb_values(lean, "Sample").unwrap_err();
        assert_eq!(error, LeanVocabularyParseError::MissingToDefraDB);
    }

    #[test]
    fn reports_malformed_arm() {
        let lean = r#"
namespace Sample

def toDefraDB : Sample -> String
  | .one "one"

end Sample
"#;

        let error = parse_lean_to_defradb_values(lean, "Sample").unwrap_err();
        assert!(matches!(
            error,
            LeanVocabularyParseError::MalformedArm {
                reason: "missing `=>`",
                ..
            }
        ));
    }

    #[test]
    fn parses_target_from_multiple_namespaces() {
        let lean = r#"
namespace Other

def toDefraDB : Other -> String
  | .one => "other"

end Other

namespace Sample

def toDefraDB : Sample -> String
  | .one => "sample"

end Sample
"#;

        assert_eq!(
            parse_lean_to_defradb_values(lean, "Sample").unwrap(),
            vec!["sample"]
        );
    }

    #[test]
    fn extracts_contract_json_between_sentinels() {
        let stdout = format!(
            "debug {{noise}}\n{}\n{{\"ok\":true}}\n{}\nmore {{noise}}\n",
            defra_agent_lean_contract::CONTRACT_JSON_BEGIN,
            defra_agent_lean_contract::CONTRACT_JSON_END
        );

        assert_eq!(
            defra_agent_lean_contract::extract_contract_json(&stdout).unwrap(),
            "{\"ok\":true}"
        );
    }

    #[test]
    fn assertion_message_identifies_sources_and_differences() {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(|| {
            assert_lean_to_defradb_vocabulary_matches(LeanVocabulary {
                lean_file: "sample.lean",
                model: NORMAL_LEAN,
                namespace: "Sample",
                rust_source: "Sample::ALL",
                rust_values: &["one", "three"],
            });
        });
        std::panic::set_hook(previous_hook);

        let panic = result.expect_err("assertion should fail");
        let message = panic_message(panic.as_ref());
        assert!(message.contains("Lean file: sample.lean"));
        assert!(message.contains("namespace: Sample"));
        assert!(message.contains("Rust vocabulary source: Sample::ALL"));
        assert!(message.contains("missing Lean values (present in Rust): [\"three\"]"));
        assert!(message.contains("extra Lean values (absent from Rust): [\"two\"]"));
    }

    fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|message| message.to_string())
            })
            .unwrap_or_else(|| "non-string panic".to_string())
    }
}
