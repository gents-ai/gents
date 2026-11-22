import Lean
import Proofs.Session.Operations
import Proofs.Store.Identity.RequestRetryBudget

namespace Request.PinnedRetryBudget
open Lean

/-- `InferenceExecution.max_request_retries` when unset. -/
def defaultMaxRequestRetries : Nat := 3

structure Outcome where
  accepted : Bool
  maxRetries : Option Nat
  error : Option String
  deriving ToJson

/-- The claiming Agent's resolved `InferenceExecution.max_request_retries`:
the stored value when present (zero allows no successors), else
`defaultMaxRequestRetries`; a negative stored value is refused and the
refusal names why. -/
structure ConfiguredInput where
  stored : Option Int
  deriving ToJson

def resolveConfigured (input : ConfiguredInput) : Outcome :=
  match input.stored with
  | none => ⟨true, some defaultMaxRequestRetries, none⟩
  | some value =>
    if value < 0 then ⟨false, none, some "max_request_retries must be non-negative"⟩
    else ⟨true, some value.toNat, none⟩

structure Input where
  normal : Bool
  configured : Nat
  retrySuccessor : Bool
  parentPin : Option Int
  deriving ToJson

/-- The successor budget a request's first claim pins. Clients and successor
writers never choose it, so neither the signed request nor its successors carry
one. A lineage takes the budget of its first claimed request: a root, or a
retry successor whose parent has no accepted pin, takes the claiming Agent's
resolved `InferenceExecution.max_request_retries` (`configured`); a successor
of a pinned parent inherits that pin, which keeps
`SessionState.reissue_preserves_parent_and_ceiling` and the background wake
redrive ceiling true of the stored pins. Auxiliary purposes (title audit,
access review) never have successors. A negative stored pin is rejected and
the rejection names why.

Decision: configuration changed while a request runs does not reach it. The
pin is written once, by the request's Node, as its own document
(`Identity.RequestRetryBudget`: no `update` or `delete` for anyone but the
Node, `@immutable` fields), right after the first claim's compare-and-set on
`execution_generation` commits; a reclaim never writes one (`claimWrite`),
and a successor inherits its parent's pin rather than the configuration
current when it is claimed. A crash between the claim and the pin leaves the
request unpinned, which `readPin` reads as `none`: its successors take the
configuration then current, never a value the working Agent chose. A retry
parent is terminal before its successor exists and its pin was written
before it ran, so the pin the successor reads cannot change before the
successor's claim commits. -/
def claimPin (input : Input) : Outcome :=
  match input.retrySuccessor, input.parentPin with
  | true, some value =>
    if value < 0 then ⟨false, none, some "retry parent max_retries must not be negative"⟩
    else ⟨true, some value.toNat, none⟩
  | _, _ => ⟨true, some (if input.normal then input.configured else 0), none⟩

/-- The pin a claim writes: the first claim writes `claimPin`, a reclaim of
the same physical request writes nothing and keeps the stored pin.
External premise: DefraDB's `@immutable` forbids every change after document
creation, so the pin is created whole by the Node once the first claim has
committed, never filled into an existing document. -/
def claimWrite (firstClaim : Bool) (input : Input) : Option Outcome :=
  if firstClaim then some (claimPin input) else none

theorem claimPin_inherits_parent (normal : Bool) (configured value : Nat) :
    claimPin ⟨normal, configured, true, some (value : Int)⟩ = ⟨true, some value, none⟩ := by
  simp [claimPin]

theorem claimPin_unpinned_lineage_takes_configuration (configured : Nat) (successor : Bool) :
    claimPin ⟨true, configured, successor, none⟩ = ⟨true, some configured, none⟩ := by
  cases successor <;> simp [claimPin]

theorem claimPin_auxiliary_root_has_no_successors (configured : Nat) (pin : Option Int) :
    claimPin ⟨false, configured, false, pin⟩ = ⟨true, some 0, none⟩ := by
  simp [claimPin]

/-- A successor of a pinned parent keeps the lineage's budget whatever the
configuration is when it is claimed. -/
theorem claimPin_successor_ignores_configuration (normal : Bool) (before after value : Nat) :
    claimPin ⟨normal, before, true, some (value : Int)⟩ =
      claimPin ⟨normal, after, true, some (value : Int)⟩ := by
  simp [claimPin]

/-- A reclaim writes no pin, so a configuration change after the first claim
never reaches a running request. -/
theorem reclaim_keeps_the_pin (input : Input) : claimWrite false input = none := rfl

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

theorem successorBudgetRemains_zero_admits_none (automated : Bool) (retryCount : Int) :
    successorBudgetRemains ⟨automated, retryCount, some 0⟩ = false := by
  simp [successorBudgetRemains]

/-- One stored pin of a request as a reader finds it: whether its signature
verifies under the request's Node DID over the pin's request and value, and
the value. -/
structure Pin where
  signedByNode : Bool
  maxRetries : Int
  deriving ToJson

/-- The pin a reader accepts: the smallest value among pins the request's Node
signed. ACP1 cannot authenticate a document's creator
(`Identity.RequestRetryBudget.an_agent_created_pin_is_the_agents`), so a pin
any other identity wrote is ignored, and should the Node ever have written
two, the smaller holds, so no reading raises a budget. -/
def readPin (pins : List Pin) : Option Int :=
  (pins.filter (·.signedByNode)).foldl
    (fun least pin => some ((least.map (min pin.maxRetries)).getD pin.maxRetries)) none

structure ReadInput where
  pins : List Pin
  deriving ToJson

def readPins (input : ReadInput) : Option Int := readPin input.pins

theorem readPin_ignores_unsigned (pins : List Pin) (value : Int) :
    readPin (pins ++ [⟨false, value⟩]) = readPin pins := by
  simp [readPin, List.filter_append]

theorem readPin_of_none_signed : readPin [⟨false, 9⟩, ⟨false, 100⟩] = none := by decide

theorem readPin_takes_the_least_signed :
    readPin [⟨true, 5⟩, ⟨false, 100⟩, ⟨true, 3⟩] = some 3 := by decide

end Request.PinnedRetryBudget
