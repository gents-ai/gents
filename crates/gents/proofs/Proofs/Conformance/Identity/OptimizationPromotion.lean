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

@[conformance_family]
def heldTransition : Family PromotionInput PromotionOutput where
  inputs := cases.map PromotionInput.ofCase
  model := promotionOutput

end Conformance.OptimizationPromotion
