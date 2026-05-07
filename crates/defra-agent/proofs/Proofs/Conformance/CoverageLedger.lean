import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Boundaries

/-!
# Conformance Coverage Ledger

Every domain emitted by `Proofs.Conformance.Contracts` must have a Rust
consumer or an explicitly accepted boundary/follow-up. Rust checks this ledger
against the generated JSON so new Lean contracts cannot remain advisory-only.
-/

namespace Conformance.Contracts

structure CoverageEntry where
  category : String
  domain : String
  consumer : String
  acceptedBoundary : String
  acceptedFollowUp : String
  deriving Repr

-- Consumer strings are documentation pointers; Rust checks domain coverage,
-- not that these names resolve to test symbols.
def consumerCoverage
    (category domain consumer : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumer := consumer
  , acceptedBoundary := ""
  , acceptedFollowUp := ""
  }

def boundaryCoverage
    (category domain acceptedBoundary : String)
    (consumer : String := "") : CoverageEntry :=
  { category := category
  , domain := domain
  , consumer := consumer
  , acceptedBoundary := acceptedBoundary
  , acceptedFollowUp := ""
  }

def followUpCoverage
    (category domain acceptedFollowUp : String) : CoverageEntry :=
  { category := category
  , domain := domain
  , consumer := ""
  , acceptedBoundary := ""
  , acceptedFollowUp := acceptedFollowUp
  }

def vocabularyCoverage : List CoverageEntry :=
  [ consumerCoverage
      "vocabulary"
      "RequestState"
      "lifecycle::tests::rust_request_lifecycle_state_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "ExecutionOrigin"
      "lifecycle::tests::rust_execution_origin_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "ProcessState"
      "runtime_status::tests::rust_process_state_vocabulary_matches_lean_model"
  , boundaryCoverage
      "vocabulary"
      "PersistenceState"
      boundaryPersistenceAbstractLifecycleId
  , boundaryCoverage
      "vocabulary"
      "PersistenceFailurePolicy"
      boundaryStorageHookFailurePolicyId
  , consumerCoverage
      "vocabulary"
      "ReconcilePhase"
      "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model"
  , boundaryCoverage
      "vocabulary"
      "StorageObservation"
      boundaryStorageObservationDaemonVisibleId
  , consumerCoverage
      "vocabulary"
      "SessionRecoveryLatestRequestState"
      "state_machine_conformance::generated_session_recovery_cases_cover_retry_guards_and_preservation"
  , consumerCoverage
      "vocabulary"
      "InferenceCallState"
      "admission::tests::rust_inference_call_state_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "InferenceCallTerminalReason"
      "admission::tests::rust_inference_call_terminal_reason_vocabulary_matches_lean_model"
  , consumerCoverage
      "vocabulary"
      "ToolRetryDisposition"
      "mcp_pool::tests::tool_retry_disposition_contract_cases_match_mcp_pool_policy"
  ]

def stateMachineCoverage : List CoverageEntry :=
  [ consumerCoverage
      "state_machine"
      "Request"
      "lifecycle::tests::request_state_machine_contract_is_complete"
  , consumerCoverage
      "state_machine"
      "Process"
      "runtime_status::tests::rust_process_state_transitions_match_lean_contract"
  , boundaryCoverage
      "state_machine"
      "Persistence.failClosed"
      boundaryStorageHookFailurePolicyId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , boundaryCoverage
      "state_machine"
      "Persistence.failOpen"
      boundaryStorageHookFailurePolicyId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , boundaryCoverage
      "state_machine"
      "StorageObservation.failClosed"
      boundaryStorageObservationDaemonVisibleId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , boundaryCoverage
      "state_machine"
      "StorageObservation.failOpen"
      boundaryStorageObservationDaemonVisibleId
      "state_machine_conformance::lean_executable_contracts_cover_initial_domains"
  , consumerCoverage
      "state_machine"
      "RuntimeReconcile"
      "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model"
  , consumerCoverage
      "state_machine"
      "SessionRecovery"
      "state_machine_conformance::generated_session_recovery_cases_cover_retry_guards_and_preservation"
  , consumerCoverage
      "state_machine"
      "InferenceCall"
      "admission::tests::rust_inference_call_transition_table_matches_lean_contract"
  ]

def caseCoverage : List CoverageEntry :=
  [ consumerCoverage
      "trigger_cases"
      "TriggerDispatch"
      "trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases"
  , consumerCoverage
      "runtime_cases"
      "RuntimeReconcileCases"
      "runtime_status::tests::runtime_status_generation_updates_match_lean_runtime_reconcile_cases"
  , consumerCoverage
      "session_recovery_cases"
      "SessionRecoveryCases"
      "state_machine_conformance::generated_session_recovery_cases_cover_retry_guards_and_preservation"
  , consumerCoverage
      "client_shell_cases"
      "ClientShellCases"
      "state_machine_conformance::generated_client_shell_cases_cover_shell_projection_contracts"
  , consumerCoverage
      "tool_cases"
      "ToolExecutionPreflight"
      "state_machine_conformance::generated_tool_execution_cases_cover_preflight_and_retry_contracts"
  , consumerCoverage
      "tool_cases"
      "ToolExecutionRetry"
      "mcp_pool::tests::tool_retry_disposition_contract_cases_match_mcp_pool_policy"
  ]

def followUpHookCoverage : List CoverageEntry :=
  []

def coverageLedger : List CoverageEntry :=
  vocabularyCoverage ++ stateMachineCoverage ++ caseCoverage ++ followUpHookCoverage

def CoverageEntry.toJson (entry : CoverageEntry) : String :=
  "{"
    ++ "\"category\":" ++ jsonString entry.category ++ ","
    ++ "\"domain\":" ++ jsonString entry.domain ++ ","
    ++ "\"consumer\":" ++ jsonString entry.consumer ++ ","
    ++ "\"accepted_boundary\":" ++ jsonString entry.acceptedBoundary ++ ","
    ++ "\"accepted_follow_up\":" ++ jsonString entry.acceptedFollowUp
    ++ "}"

def coverageLedgerJson : String :=
  jsonArray (coverageLedger.map CoverageEntry.toJson)

end Conformance.Contracts
