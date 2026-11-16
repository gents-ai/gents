import Proofs.Conformance.ContractCases.Runtime
import Proofs.Conformance.Contracts.Machines.Catalog
import Proofs.Conformance.Family

namespace Conformance.ClientAgentReadiness

open Lean Conformance.ContractCases

deriving instance ToJson for ProcessState, RuntimeState.AgentUnavailableReason

structure ReadinessInput where
  name : String
  observationPresent : Bool
  observationKind : String
  processState : ProcessState
  activeGeneration : Nat
  routerGeneration : Nat
  runnable : Bool
  unavailable : Bool
  startupDemoted : Bool
  runtimeUnavailableReason : RuntimeState.AgentUnavailableReason
  storageUnavailable : Bool
  deriving ToJson

def readinessInput (name observationKind : String) (processState : ProcessState)
    (activeGeneration routerGeneration : Nat) (runnable unavailable startupDemoted : Bool)
    (runtimeUnavailableReason : RuntimeState.AgentUnavailableReason)
    (storageUnavailable : Bool := false) : ReadinessInput :=
  { name, observationPresent := observationKind != "missing", observationKind, processState
    activeGeneration, routerGeneration, runnable, unavailable, startupDemoted
    runtimeUnavailableReason, storageUnavailable }

structure ReadinessOutput where
  state : String
  reason : Option String
  runtimeAdmissible : Bool
  deriving ToJson

/-- A predecessor's fully ready row after a restarting runtime's claim. -/
def restartClaimInput : ReadinessInput :=
  readinessInput "restart_claim_demotes_stale_ready" "observed" restartClaimedProcessState 4 4
    true false false .backendTemporarilyUnavailable

def projectionModel (i : ReadinessInput) : ReadinessOutput :=
  let c := clientAgentReadinessCase i.name i.observationKind i.processState i.activeGeneration
    i.routerGeneration i.runnable i.unavailable i.startupDemoted i.runtimeUnavailableReason
    i.storageUnavailable
  { state := c.expectedState, reason := c.expectedReason
    runtimeAdmissible := c.expectedRuntimeAdmissible }

@[conformance_family]
def projection : Family ReadinessInput ReadinessOutput where
  inputs :=
  [ readinessInput "runtime_ready_same_generation" "observed" .ready 4 4 true false false .backendTemporarilyUnavailable
  , readinessInput "runtime_explicitly_unavailable" "observed" .ready 4 4 false true false .backendTemporarilyUnavailable
  , readinessInput "runtime_unavailable_wins_overlap" "observed" .ready 4 4 true true false .backendTemporarilyUnavailable
  , readinessInput "startup_demotion_overrides_runnable" "observed" .ready 4 4 true false true .backendTemporarilyUnavailable
  , readinessInput "missing_runtime_observation" "missing" .ready 4 4 true false false .backendTemporarilyUnavailable
  , readinessInput "malformed_runtime_observation" "malformed" .ready 4 4 true false false .backendTemporarilyUnavailable
  , readinessInput "unsupported_runtime_observation" "unsupported_version" .ready 4 4 true false false .backendTemporarilyUnavailable
  , readinessInput "runtime_process_recovering" "observed" .recovering 4 4 true false false .backendTemporarilyUnavailable
  , readinessInput "runtime_process_uninitialized" "observed" .uninitialized 4 4 true false false .backendTemporarilyUnavailable
  , readinessInput "runtime_process_shutting_down" "observed" .shuttingDown 4 4 true false false .backendTemporarilyUnavailable
  , readinessInput "runtime_process_shutdown" "observed" .shutdown 4 4 true false false .backendTemporarilyUnavailable
  , readinessInput "router_generation_stale" "observed" .ready 5 4 true false false .backendTemporarilyUnavailable
  , readinessInput "zero_generation_is_stale" "observed" .ready 0 0 true false false .backendTemporarilyUnavailable
  , readinessInput "agent_absent_from_runtime_projection" "observed" .ready 4 4 false false false .backendTemporarilyUnavailable
  , readinessInput "disabled_agent_is_unavailable" "observed" .ready 4 4 false true false .agentDisabled
  , readinessInput "invalid_runtime_configuration_is_unavailable" "observed" .ready 4 4 false true false .runtimeConfigurationInvalid
  , readinessInput "missing_backend_is_unavailable" "observed" .ready 4 4 false true false .backendNotConfigured
  , readinessInput "disabled_backend_is_unavailable" "observed" .ready 4 4 false true false .backendDisabled
  , readinessInput "missing_credential_is_unavailable" "observed" .ready 4 4 false true false .credentialsRequired
  , readinessInput "rejected_subscription_credential_is_unavailable" "observed" .ready 4 4 false true false .credentialsRequired
  , readinessInput "subscription_not_entitled_is_unavailable" "observed" .ready 4 4 false true false .subscriptionNotEntitled
  , readinessInput "invalid_inference_profile_is_unavailable" "observed" .ready 4 4 false true false .inferenceProfileInvalid
  , readinessInput "invalid_tool_configuration_is_unavailable" "observed" .ready 4 4 false true false .toolConfigurationInvalid
  , readinessInput "invalid_tool_surface_is_unavailable" "observed" .ready 4 4 false true false .toolSurfaceUnavailable
  , restartClaimInput
  , readinessInput "storage_fault_closes_a_ready_row" "observed" .ready 4 4 true false false .backendTemporarilyUnavailable
      (storageUnavailable := true)
  , readinessInput "storage_fault_closes_a_missing_row" "missing" .ready 4 4 true false false .backendTemporarilyUnavailable
      (storageUnavailable := true)
  ]
  model := projectionModel

structure ClaimedRowOutput where
  state : String
  reason : Option String
  deriving ToJson

/-- The runtime's startup claim is a different production owner from the
client projection, so it replays its own family over the same modeled row:
every reader must see the claimed row as unknown, for the model's reason. -/
@[conformance_family]
def restartClaim : Family ReadinessInput ClaimedRowOutput where
  inputs := [restartClaimInput]
  model := fun i =>
    let o := projectionModel i
    { state := o.state, reason := o.reason }

end Conformance.ClientAgentReadiness
