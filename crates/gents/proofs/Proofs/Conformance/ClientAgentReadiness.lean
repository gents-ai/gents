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
  deriving ToJson

def readinessInput (name observationKind : String) (processState : ProcessState)
    (activeGeneration routerGeneration : Nat) (runnable unavailable startupDemoted : Bool)
    (runtimeUnavailableReason : RuntimeState.AgentUnavailableReason) : ReadinessInput :=
  { name, observationPresent := observationKind != "missing", observationKind, processState
    activeGeneration, routerGeneration, runnable, unavailable, startupDemoted
    runtimeUnavailableReason }

structure ReadinessOutput where
  state : String
  reason : Option String
  runtimeAdmissible : Bool
  deriving ToJson

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
  ]
  model := fun i =>
    let c := clientAgentReadinessCase i.name i.observationKind i.processState i.activeGeneration
      i.routerGeneration i.runnable i.unavailable i.startupDemoted i.runtimeUnavailableReason
    { state := c.expectedState, reason := c.expectedReason
      runtimeAdmissible := c.expectedRuntimeAdmissible }

end Conformance.ClientAgentReadiness
