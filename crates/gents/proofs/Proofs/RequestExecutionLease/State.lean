import Proofs.Request.State

/-!
# Request execution lease state

The generation parameter is deliberately abstract: claims, recovery, and revocation
require a fresh opaque value; output never changes lease expiry or recovery authority.
The owner's explicit cadence-bounded deadline CAS requires a live lease at admission;
native durable commit can occur later (see `ObservedTransaction`).
-/

namespace RequestExecutionLease

inductive Outcome where
  | completed
  | failed
  | interrupted
  | dead
  | superseded
  deriving DecidableEq, Repr

/-- Native transaction commit; an observing replica cannot admit writes or recover work. -/
inductive Boundary where
  | mutationWriteGate
  | validatedTransaction
  | observingReplica
  deriving DecidableEq, Repr

inductive Lease (Generation : Type) where
  | vacant
  | active (generation : Generation) (duration explicitDeadline : Time)
  /-- Explicitly dropped work only; expiry recovery swaps generation atomically from `active`, never committing this state. -/
  | recoverable (generation : Generation) (duration explicitDeadline : Time)
  | terminal (generation : Generation) (outcome : Outcome)
  deriving DecidableEq, Repr

structure World (Generation : Type) where
  request : RequestState
  lease : Lease Generation
  usedGenerations : List Generation
  now : Time
  continuationRequired : Bool
  tokenChargeRequired : Bool
  continuationCount : Nat
  tokenChargeCount : Nat
  deriving DecidableEq, Repr

def initial (Generation : Type) : World Generation :=
  { request := .pending
  , lease := .vacant
  , usedGenerations := []
  , now := 0
  , continuationRequired := false
  , tokenChargeRequired := false
  , continuationCount := 0
  , tokenChargeCount := 0
  }

def Outcome.requestState : Outcome → RequestState
  | .completed => .completed
  | .failed => .failed
  | .interrupted => .interrupted
  | .dead => .dead
  | .superseded => .superseded

def terminalAgreement {Generation : Type} (world : World Generation) : Prop :=
  match world.lease with
  | .terminal _ outcome => world.request = outcome.requestState
  | _ => True

def terminalEffectsBounded {Generation : Type} (world : World Generation) : Prop :=
  world.continuationCount ≤ 1 ∧ world.tokenChargeCount ≤ 1

def fresh {Generation : Type} [DecidableEq Generation]
    (world : World Generation) (generation : Generation) : Prop :=
  generation ∉ world.usedGenerations

instance {Generation : Type} [DecidableEq Generation]
    (world : World Generation) (generation : Generation) :
    Decidable (fresh world generation) := by
  unfold fresh
  infer_instance

def effectiveExpiry {Generation : Type}
    (world : World Generation) : Time :=
  match world.lease with
  | .active _ _ explicitDeadline
  | .recoverable _ _ explicitDeadline => explicitDeadline
  | .vacant | .terminal _ _ => 0

def admitted {Generation : Type} [DecidableEq Generation]
    (world : World Generation) (boundary : Boundary) (generation : Generation) : Prop :=
  boundary = .mutationWriteGate ∧
    match world.lease with
    | .active owner _ deadline => owner = generation ∧ world.now < deadline
    | _ => False

instance {Generation : Type} [DecidableEq Generation]
    (world : World Generation) (boundary : Boundary) (generation : Generation) :
    Decidable (admitted world boundary generation) := by
  unfold admitted
  cases boundary <;> cases hlease : world.lease <;> simp <;> infer_instance

/-- States where the owned completion loop may keep its lease alive; a mailbox handoff completes the producer without holding a lease for the later reply request. -/
def renewableLifecycle : RequestState → Prop
  | .claimed | .processing => True
  | _ => False

instance (request : RequestState) : Decidable (renewableLifecycle request) := by
  cases request <;> unfold renewableLifecycle <;> infer_instance

def canFinalize {Generation : Type} (world : World Generation)
    (outcome : Outcome) : Prop :=
  match outcome with
  | .completed => world.request = .processing
  | .failed | .interrupted | .dead | .superseded =>
      world.request = .claimed ∨ world.request = .processing

instance {Generation : Type} (world : World Generation) (outcome : Outcome) :
    Decidable (canFinalize world outcome) := by
  unfold canFinalize
  cases outcome <;> infer_instance

def commitTerminalEffects {Generation : Type}
    (world : World Generation) : World Generation :=
  { world with
    continuationCount := world.continuationCount + (if world.continuationRequired then 1 else 0)
    tokenChargeCount := world.tokenChargeCount + (if world.tokenChargeRequired then 1 else 0) }

def terminalize {Generation : Type}
    (world : World Generation) (generation : Generation)
    (outcome : Outcome) : World Generation :=
  commitTerminalEffects
    { world with
      request := outcome.requestState
      lease := .terminal generation outcome }

/-- EOF is transport observation, not evidence of a completed provider turn. -/
def providerEofIsFailure (sawExplicitFinal : Bool) : Bool := !sawExplicitFinal

end RequestExecutionLease
