import Proofs.Conformance.ContractCases.Types
import Proofs.Compaction.DurableReduction

namespace Conformance.ContractCases

open Compaction.DurableReduction

structure DurableReductionCase where
  name : String
  requestDocId : Nat
  turnIndex : Nat
  ordinal : Nat
  checkpoint : Nat
  claimCommit : Nat
  priorCheckpoint : Option Nat
  priorClaimCommit : Option Nat
  pairClosed : Bool
  inferenceCites : Bool
  titleCites : Bool
  consumed : Bool
  outcome : String
  durableAfter : Bool
  sendPermitted : Bool
  deriving Repr

private def key (requestDocId turnIndex ordinal : Nat) : ReductionKey :=
  { agentDid := 7, sessionId := 11, requestDocId, turnIndex, ordinal }

private def fact (checkpoint claimCommit : Nat) (pairClosed : Bool) : Fact :=
  { claimCommit
  , sourceBoundary := { value := 41 }
  , sourceProjection := { value := 42 }
  , checkpoint := { value := checkpoint }
  , producerCall := some 99
  , parent := none
  , pairClosed
  }

private def reductionCase (name : String) (requestDocId turnIndex ordinal checkpoint claimCommit : Nat)
    (priorCheckpoint priorClaimCommit : Option Nat) (pairClosed inferenceCites titleCites : Bool) :
    DurableReductionCase :=
  let reductionKey := key requestDocId turnIndex ordinal
  let scenario : Scenario :=
    { key := reductionKey
    , fact := fact checkpoint claimCommit pairClosed
    , prior := priorCheckpoint.map (fun prior => fact prior (priorClaimCommit.getD claimCommit) pairClosed)
    }
  let captures : List CaptureCitation :=
    (if inferenceCites then [{ kind := .inference, reductionKeys := [reductionKey] }] else []) ++
    (if titleCites then [{ kind := .title, reductionKeys := [reductionKey] }] else [])
  { name, requestDocId, turnIndex, ordinal, checkpoint, claimCommit, priorCheckpoint,
    priorClaimCommit, pairClosed, inferenceCites, titleCites
  , outcome := scenario.outcome.toContract
  , durableAfter := scenario.durableAfter
  , sendPermitted := scenario.sendPermitted
  , consumed := consumedBy reductionKey captures
  }

def durableReductionCases : List DurableReductionCase :=
  [ reductionCase "fresh_reduction_is_durable_before_send" 23 0 1 100 51 none none true false false
  , reductionCase "identical_redelivery_is_idempotent" 23 0 1 100 51 (some 100) (some 51) true false true
  , reductionCase "conflicting_rebinding_blocks_send" 23 0 1 100 51 (some 101) (some 51) true true false
  , reductionCase "pair_open_checkpoint_blocks_send" 23 0 1 100 51 none none false false false
  , reductionCase "later_claim_can_create_next_ordered_fact" 23 4 2 102 52 none none true false false
  , reductionCase "concurrent_request_is_a_distinct_fact" 24 0 1 103 51 none none true false false
  ]

theorem durableReductionCases_pinned :
    durableReductionCases.map
      (fun row => (row.name, row.outcome, row.durableAfter, row.sendPermitted)) =
      [ ("fresh_reduction_is_durable_before_send", "fresh", true, true)
      , ("identical_redelivery_is_idempotent", "idempotent", true, true)
      , ("conflicting_rebinding_blocks_send", "conflict", false, false)
      , ("pair_open_checkpoint_blocks_send", "pair_open", false, false)
      , ("later_claim_can_create_next_ordered_fact", "fresh", true, true)
      , ("concurrent_request_is_a_distinct_fact", "fresh", true, true)
      ] := by
  rfl

theorem durableReductionCases_no_fail_open :
    durableReductionCases.all
      (fun row => !row.sendPermitted || (row.durableAfter && row.pairClosed)) = true := by
  rfl

theorem durableReductionCases_consumption_is_inference_only :
    durableReductionCases.map (fun row => (row.name, row.consumed)) =
      [ ("fresh_reduction_is_durable_before_send", false)
      , ("identical_redelivery_is_idempotent", false)
      , ("conflicting_rebinding_blocks_send", true)
      , ("pair_open_checkpoint_blocks_send", false)
      , ("later_claim_can_create_next_ordered_fact", false)
      , ("concurrent_request_is_a_distinct_fact", false)
      ] := by
  rfl

end Conformance.ContractCases
