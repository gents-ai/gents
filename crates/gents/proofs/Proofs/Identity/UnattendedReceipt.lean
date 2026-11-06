import Proofs.Identity.Household
import Lean

namespace Identity.UnattendedReceipt

inductive DurableState where
  | absent | pending | admitted
  deriving BEq, DecidableEq, Repr

inductive NativeWitness where
  | absent | pending | admitted | unavailable
  deriving BEq, DecidableEq, Repr

/-- Contract for a prospective native, transaction-scoped receipt witness.
The native owner authenticates the selected actor and checks its named invoke
permission and read authority on the exact physical consumer and source. It
resolves one complete unique-index key without exposing the hidden document,
its owner, payload, request or session. An admitted Task witness requires the
canonical receipt and request to be durably linked in the same committed
delivery; an admitted callback witness requires the native-authenticated
claim, not merely a pending invocation. A retry that retained its prior claim
is admitted even when its current lifecycle says pending. A pending witness
exposes only a never-claimed state. Absent requires complete current schema/index coverage and an
observation conflict-fenced against competing creation and claim. Mismatched
physical consumers, unauthorized actors, missing provenance, ambiguous records,
revocation or unavailable native guarantees yield unavailable. This is a
native-interface obligation, not an application authorization interpreter.
For an absent key, authenticated provenance means that the native index can
authenticate any matching committed publisher, not that an absent document has
provenance. -/
def observe (state : DurableState) (actorAuthorized exactConsumer sourceVisible
    completeIndex authenticatedProvenance conflictFenced : Bool) : NativeWitness :=
  if actorAuthorized && exactConsumer && sourceVisible && completeIndex &&
      authenticatedProvenance && conflictFenced then
    match state with
    | .absent => .absent
    | .pending => .pending
    | .admitted => .admitted
  else .unavailable

inductive NextAction where
  | hold | create | retirePending | advance
  deriving BEq, DecidableEq, Repr

/-- Retiring an old pending invocation is a user-authorized native conditional
delete on the exact observed version, atomically fenced against claim and
reassignment. It does not transfer its private input, rewrite its native owner,
or authorize a second execution. Re-delivery remains subject to fresh Agent
admission. If the cursor already passed the pending arrival, retirement must
atomically rewind it before that arrival using a retained native source position;
all later arrivals encountered on replay still need the same native witness.
Unavailable source history or an unfenced rewind prevents retirement. Admitted
work only advances the cursor and continues as its original Agent; no later
requester or configuration check recalls that work. -/
def nextAction (witness : NativeWitness) (ownerAuthorized retireFenced : Bool) : NextAction :=
  match witness with
  | .absent => .create
  | .pending => if ownerAuthorized && retireFenced then .retirePending else .hold
  | .admitted => .advance
  | .unavailable => .hold

theorem unavailable_never_creates (ownerAuthorized retireFenced : Bool) :
    nextAction .unavailable ownerAuthorized retireFenced = .hold := by rfl

theorem admitted_never_creates (ownerAuthorized retireFenced : Bool) :
    nextAction .admitted ownerAuthorized retireFenced = .advance := by rfl

theorem pending_without_fenced_retirement_never_creates (ownerAuthorized : Bool) :
    nextAction .pending ownerAuthorized false = .hold := by
  simp [nextAction]

theorem pending_without_owner_never_retires (retireFenced : Bool) :
    nextAction .pending false retireFenced = .hold := by rfl

theorem pending_neither_creates_nor_advances (ownerAuthorized retireFenced : Bool) :
    nextAction .pending ownerAuthorized retireFenced ≠ .create ∧
    nextAction .pending ownerAuthorized retireFenced ≠ .advance := by
  cases ownerAuthorized <;> cases retireFenced <;> decide

/-- A visible callback invocation is only a checkpoint receipt after the
existing claim transaction retained both its claim time and configuration.
These markers remain when admitted work is retried. They do not prove anything
about an invocation hidden from the current actor. -/
def visibleCallbackClaim (claimTime retainedConfiguration : Bool) : Bool :=
  claimTime && retainedConfiguration

theorem pending_creation_cannot_checkpoint :
    visibleCallbackClaim false false = false := by rfl

theorem retry_with_retained_claim_can_checkpoint :
    visibleCallbackClaim true true = true := by rfl

theorem incomplete_index_never_proves_absence
    (actorAuthorized exactConsumer sourceVisible authenticatedProvenance conflictFenced : Bool) :
    observe .absent actorAuthorized exactConsumer sourceVisible false authenticatedProvenance conflictFenced = .unavailable := by
  simp [observe]

theorem unauthenticated_evidence_is_unavailable (state : DurableState)
    (exactConsumer sourceVisible completeIndex authenticatedProvenance conflictFenced : Bool) :
    observe state false exactConsumer sourceVisible completeIndex authenticatedProvenance conflictFenced = .unavailable := by
  rfl

theorem detached_observation_is_unavailable (state : DurableState)
    (actorAuthorized exactConsumer sourceVisible completeIndex authenticatedProvenance : Bool) :
    observe state actorAuthorized exactConsumer sourceVisible completeIndex authenticatedProvenance false = .unavailable := by
  simp [observe]

/-- The currently available owner observation reports ordinary visibility,
not a proof of nonexistence; a hidden receipt and an absent receipt agree. -/
def ordinaryVisible (present readable : Bool) : Bool := present && readable

theorem hidden_and_absent_are_indistinguishable :
    ordinaryVisible true false = ordinaryVisible false false := by rfl

def stateString : DurableState → String
  | .absent => "absent" | .pending => "pending" | .admitted => "admitted"

def witnessString : NativeWitness → String
  | .absent => "absent" | .pending => "pending" | .admitted => "admitted" | .unavailable => "unavailable"

def actionString : NextAction → String
  | .hold => "hold" | .create => "create" | .retirePending => "retire_pending" | .advance => "advance"

def cases : List Lean.Json :=
  let inputs : List (String × DurableState × Bool × Bool × Bool × Bool × Bool × Bool × Bool × Bool) :=
    [("absent", .absent, true, true, true, true, true, true, true, true),
     ("admitted", .admitted, true, true, true, true, true, true, true, true),
     ("pending_no_retirement", .pending, true, true, true, true, true, true, false, false),
     ("pending_fenced_retirement", .pending, true, true, true, true, true, true, true, true),
     ("untrusted_publisher", .admitted, true, true, true, true, false, true, true, true),
     ("wrong_consumer", .admitted, true, false, true, true, true, true, true, true),
     ("source_hidden", .admitted, true, true, false, true, true, true, true, true),
     ("incomplete_index", .absent, true, true, true, false, true, true, true, true),
     ("unfenced", .admitted, true, true, true, true, true, false, true, true),
     ("foreign_actor", .admitted, false, true, true, true, true, true, true, true)]
  inputs.map fun (name, state, actor, consumer, source, index, provenance, fence, owner, retireFence) =>
    let witness := observe state actor consumer source index provenance fence
    Lean.Json.mkObj [("name", Lean.toJson name), ("state", Lean.toJson (stateString state)),
      ("actor_authorized", Lean.toJson actor), ("exact_consumer", Lean.toJson consumer),
      ("source_visible", Lean.toJson source), ("complete_index", Lean.toJson index),
      ("authenticated_provenance", Lean.toJson provenance), ("conflict_fenced", Lean.toJson fence),
      ("owner_authorized", Lean.toJson owner), ("retire_fenced", Lean.toJson retireFence),
      ("witness", Lean.toJson (witnessString witness)),
      ("action", Lean.toJson (actionString (nextAction witness owner retireFence)))]

end Identity.UnattendedReceipt
