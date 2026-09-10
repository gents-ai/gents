import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

def r5CrossPrincipalCase
    (name route parentPrincipal childPrincipal parentRequestId parentToolCallId
      childRequestId targetBehaviorId : String)
    (crossPrincipalRoutingFired samePrincipalFallback unclaimedDeadlineSet : Bool) :
    R5CrossPrincipalCase :=
  { name := name
  , route := route
  , action := "spawn_subagent"
  , parentPrincipal := parentPrincipal
  , childPrincipal := childPrincipal
  , parentRequestId := parentRequestId
  , parentToolCallId := parentToolCallId
  , childRequestId := childRequestId
  , targetBehaviorId := targetBehaviorId
  , awaitMode := "background"
  , cancelPolicy := "cascade"
  , parentTriggerPersisted := true
  , childMaterialized := true
  , childOwnedByTargetPrincipal := true
  , causedByParentRequestIdMatches := true
  , causedByParentToolCallIdMatches := true
  , causedByTriggerKind := "subagent"
  , crossPrincipalRoutingFired := crossPrincipalRoutingFired
  , samePrincipalFallback := samePrincipalFallback
  , unclaimedDeadlineSet := unclaimedDeadlineSet
  }

def r5CrossPrincipalCases : List R5CrossPrincipalCase :=
  [ r5CrossPrincipalCase
      "r5_cross_principal_background_claim_materializes_child"
      "cross_principal"
      "did:principal-a"
      "did:principal-b"
      "r5-lean-cross-parent"
      "r5-lean-cross-tool"
      "runtime_generated"
      "r5-lean-cross-child-behavior"
      true
      false
      true
  , r5CrossPrincipalCase
      "r5_same_principal_background_fallback_materializes_child"
      "same_principal"
      "did:principal-a"
      "did:principal-a"
      "r5-lean-local-parent"
      "r5-lean-local-tool"
      "runtime_generated"
      "r5-lean-local-child-behavior"
      false
      true
      true
  ]

end Conformance.ContractCases
