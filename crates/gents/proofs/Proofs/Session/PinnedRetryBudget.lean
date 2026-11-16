import Lean
import Proofs.Session.Operations

namespace Request.PinnedRetryBudget
open Lean

/-- `InferenceExecution.max_request_retries` when unset. -/
def defaultMaxRequestRetries : Nat := 3

structure Input where
  normal : Bool
  configured : Nat
  retrySuccessor : Bool
  parentPin : Option Int
  deriving ToJson

structure Outcome where
  accepted : Bool
  maxRetries : Option Nat
  error : Option String
  deriving ToJson

/-- The successor budget (`AgentRequest.max_retries`) a request's first claim
pins; later claims never rewrite it. Clients and successor writers never
choose it, so neither the signed request nor its successors carry one. A
lineage takes the budget of its first claimed request: a root, or a retry
successor whose parent never reached a claim, takes the claiming Agent's
resolved `InferenceExecution.max_request_retries` (`configured`); a successor
of a pinned parent inherits that pin, which keeps
`SessionState.reissue_preserves_parent_and_ceiling` and the background wake
redrive ceiling true of the stored rows. Auxiliary purposes (title audit,
access review) never have successors. A negative stored pin is rejected and
the rejection names why. -/
def claimPin (input : Input) : Outcome :=
  match input.retrySuccessor, input.parentPin with
  | true, some value =>
    if value < 0 then ⟨false, none, some "retry parent max_retries must not be negative"⟩
    else ⟨true, some value.toNat, none⟩
  | _, _ => ⟨true, some (if input.normal then input.configured else 0), none⟩

theorem claimPin_inherits_parent (normal : Bool) (configured value : Nat) :
    claimPin ⟨normal, configured, true, some (value : Int)⟩ = ⟨true, some value, none⟩ := by
  simp [claimPin]

theorem claimPin_unpinned_lineage_takes_configuration (configured : Nat) (successor : Bool) :
    claimPin ⟨true, configured, successor, none⟩ = ⟨true, some configured, none⟩ := by
  cases successor <;> simp [claimPin]

theorem claimPin_auxiliary_root_has_no_successors (configured : Nat) (pin : Option Int) :
    claimPin ⟨false, configured, false, pin⟩ = ⟨true, some 0, none⟩ := by
  simp [claimPin]

structure BudgetInput where
  automated : Bool
  retryCount : Int
  maxRetries : Option Int
  deriving ToJson

/-- Whether a terminal request's lineage may take one more successor. A pinned
budget admits one while `0 ≤ retryCount < maxRetries`
(`SessionOperations.budgetRemains`). A request without a pin never reached a
claim and consumed no attempt: an operator reissue may follow it, and its
successor's claim pins the lineage budget; automated background redrive does
not, because such a request was refused before it ran and replaying it would
be refused again. -/
def successorBudgetRemains (input : BudgetInput) : Bool :=
  match input.maxRetries with
  | none => !input.automated
  | some maxRetries => decide (0 ≤ input.retryCount) && decide (input.retryCount < maxRetries)

theorem successorBudgetRemains_pinned (automated : Bool) (retryCount maxRetries : Nat) :
    successorBudgetRemains ⟨automated, retryCount, some maxRetries⟩ =
      SessionOperations.budgetRemains retryCount maxRetries := by
  simp [successorBudgetRemains, SessionOperations.budgetRemains]

end Request.PinnedRetryBudget
