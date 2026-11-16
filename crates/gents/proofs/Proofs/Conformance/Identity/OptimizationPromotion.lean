import Proofs.Store.Identity.OptimizationPromotion
import Proofs.Conformance.Family

namespace Conformance.OptimizationPromotion

open Lean Identity.OptimizationPromotion

structure PromotionInput where
  name : String
  actor : Option String
  action : String
  targetAllowed : Bool
  journalAllowed : Bool
  expectedLength : Bool
  commit : Bool
  deriving ToJson

structure JournalWire where
  action : String
  principal : String
  deriving ToJson

structure PromotionOutput where
  target : String
  journal : List JournalWire
  deriving ToJson

def PromotionInput.ofCase (c : Case) : PromotionInput :=
  { name := c.name, actor := c.actor, action := c.action, targetAllowed := c.targetAllowed
    journalAllowed := c.journalAllowed, expectedLength := c.expectedLength, commit := c.commit }

def promotionOutput (i : PromotionInput) : PromotionOutput :=
  let after := transition ⟨"before", []⟩ i.actor i.action "after" i.targetAllowed
    i.journalAllowed i.expectedLength i.commit
  { target := after.target
    journal := after.journal.map fun (action, principal) => { action, principal } }

/-- A held evaluator harness drives these cases: the held user promotes and reverts,
an authenticated reader holds target but not job update permission, and an
anonymous caller has no actor. -/
def heldCaseNames : List String :=
  ["held-user-promote", "held-user-revert", "reader-cannot-update-job", "missing-actor"]

@[conformance_family]
def heldTransition : Family PromotionInput PromotionOutput where
  inputs := (cases.filter fun c => c.name ∈ heldCaseNames).map PromotionInput.ofCase
  model := promotionOutput

/-- model-production-gap: no production path withholds target update permission from a
caller that may update the job, forces the journal compare-and-set to observe a stale
length, or fails the commit after both writes are staged. -/
@[conformance_family]
def heldTransitionUnreached : Family PromotionInput PromotionOutput where
  inputs := (cases.filter fun c => c.name ∉ heldCaseNames).map PromotionInput.ofCase
  model := promotionOutput

end Conformance.OptimizationPromotion
