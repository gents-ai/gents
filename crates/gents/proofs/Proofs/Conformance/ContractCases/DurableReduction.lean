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
  priorCheckpoint : Option Nat
  pairClosed : Bool
  outcome : String
  durableAfter : Bool
  sendPermitted : Bool
  deriving Repr

private def key (requestDocId turnIndex ordinal : Nat) : ReductionKey :=
  { agentDid := 7, sessionId := 11, requestDocId, turnIndex, ordinal }

private def fact (checkpoint : Nat) (pairClosed : Bool) : Fact :=
  { sourceBoundary := { value := 41 }
  , sourceProjection := { value := 42 }
  , checkpoint := { value := checkpoint }
  , producerCall := some 99
  , parent := none
  , pairClosed
  }

private def reductionCase (name : String) (requestDocId turnIndex ordinal checkpoint : Nat)
    (priorCheckpoint : Option Nat) (pairClosed : Bool) : DurableReductionCase :=
  let scenario : Scenario :=
    { key := key requestDocId turnIndex ordinal
    , fact := fact checkpoint pairClosed
    , prior := priorCheckpoint.map (fun prior => fact prior pairClosed)
    }
  { name, requestDocId, turnIndex, ordinal, checkpoint, priorCheckpoint, pairClosed
  , outcome := scenario.outcome.toContract
  , durableAfter := scenario.durableAfter
  , sendPermitted := scenario.sendPermitted
  }

def durableReductionCases : List DurableReductionCase :=
  [ reductionCase "fresh_reduction_is_durable_before_send" 23 0 1 100 none true
  , reductionCase "identical_redelivery_is_idempotent" 23 0 1 100 (some 100) true
  , reductionCase "conflicting_rebinding_blocks_send" 23 0 1 100 (some 101) true
  , reductionCase "pair_open_checkpoint_blocks_send" 23 0 1 100 none false
  , reductionCase "second_turn_is_a_distinct_ordered_fact" 23 4 2 102 none true
  , reductionCase "concurrent_request_is_a_distinct_fact" 24 0 1 103 none true
  ]

theorem durableReductionCases_pinned :
    durableReductionCases.map
      (fun row => (row.name, row.outcome, row.durableAfter, row.sendPermitted)) =
      [ ("fresh_reduction_is_durable_before_send", "fresh", true, true)
      , ("identical_redelivery_is_idempotent", "idempotent", true, true)
      , ("conflicting_rebinding_blocks_send", "conflict", false, false)
      , ("pair_open_checkpoint_blocks_send", "pair_open", false, false)
      , ("second_turn_is_a_distinct_ordered_fact", "fresh", true, true)
      , ("concurrent_request_is_a_distinct_fact", "fresh", true, true)
      ] := by
  rfl

theorem durableReductionCases_no_fail_open :
    durableReductionCases.all
      (fun row => !row.sendPermitted || (row.durableAfter && row.pairClosed)) = true := by
  rfl

end Conformance.ContractCases
