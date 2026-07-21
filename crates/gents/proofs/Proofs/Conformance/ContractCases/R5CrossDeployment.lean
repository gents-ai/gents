import Proofs.Conformance.ContractCases.Types

/-!
# R5 Cross-Deployment Subagent Conformance Cases

Finite witnesses for the agent-facing R5 dispatch lifecycle. The rows pin the
production contract around `spawn_subagent`: a parent bridge row is persisted,
the child request materializes with parent linkage, and routing either crosses
deployment boundaries or falls back to same-deployment materialization.
-/

namespace Conformance.ContractCases

def r5CrossDeploymentCase
    (name route parentDeployment childDeployment parentRequestId parentToolCallId
      childRequestId targetBehaviorId : String)
    (crossDeploymentRoutingFired singleDeploymentFallback unclaimedDeadlineSet : Bool) :
    R5CrossDeploymentCase :=
  { name := name
  , route := route
  , action := "spawn_subagent"
  , parentDeployment := parentDeployment
  , childDeployment := childDeployment
  , parentRequestId := parentRequestId
  , parentToolCallId := parentToolCallId
  , childRequestId := childRequestId
  , targetBehaviorId := targetBehaviorId
  , awaitMode := "background"
  , cancelPolicy := "cascade"
  , parentTriggerPersisted := true
  , childMaterialized := true
  , childOwnedByTargetDeployment := true
  , causedByParentRequestIdMatches := true
  , causedByParentToolCallIdMatches := true
  , causedByTriggerKind := "subagent"
  , crossDeploymentRoutingFired := crossDeploymentRoutingFired
  , singleDeploymentFallback := singleDeploymentFallback
  , unclaimedDeadlineSet := unclaimedDeadlineSet
  }

def r5CrossDeploymentCases : List R5CrossDeploymentCase :=
  [ r5CrossDeploymentCase
      "r5_cross_deployment_background_claim_materializes_child"
      "cross_deployment"
      "deployment_a"
      "deployment_b"
      "r5-lean-cross-parent"
      "r5-lean-cross-tool"
      "runtime_generated"
      "r5-lean-cross-child-behavior"
      true
      false
      true
  , r5CrossDeploymentCase
      "r5_single_deployment_background_fallback_materializes_child"
      "single_deployment"
      "deployment_a"
      "deployment_a"
      "r5-lean-local-parent"
      "r5-lean-local-tool"
      "runtime_generated"
      "r5-lean-local-child-behavior"
      false
      true
      true
  ]

end Conformance.ContractCases
