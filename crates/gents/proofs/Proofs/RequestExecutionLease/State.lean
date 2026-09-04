import Proofs.Basic

/-!
# Request execution lease state

The generation parameter is deliberately abstract.  The machine may compare
ownership generations for equality and remember which values were used, but it
cannot order, increment, or otherwise derive a successor.  A caller must
provide a genuinely fresh opaque value when claiming or recovering work.
-/

namespace RequestExecutionLease

inductive RequestPhase where
  | pending
  | claimed
  | processing
  | completed
  | failed
  | interrupted
  | dead
  | superseded
  deriving DecidableEq, Repr

inductive ResponsePhase where
  | absent
  | streaming
  | completed
  | failed
  | interrupted
  deriving DecidableEq, Repr

inductive Outcome where
  | completed
  | failed
  | interrupted
  | dead
  | superseded
  deriving DecidableEq, Repr

inductive ProgressKind where
  | response
  | tool
  | transcript
  deriving DecidableEq, Repr

inductive Lease (Generation : Type) where
  | vacant
  | active (generation : Generation) (deadline : Time)
  | recoverable (generation : Generation)
  | terminal (generation : Generation) (outcome : Outcome)
  deriving DecidableEq, Repr

/-- `continuationCount` and `tokenChargeCount` count the contested terminal
side effects owned by this lease, not all provider turns in the request.  The
request-wide token ledger is modeled separately by `PromptAssembly.AggregateBudget`.
They are explicit naturals so a duplicate terminal winner would be observable
as a value greater than one. -/
structure World (Generation : Type) where
  request : RequestPhase
  response : ResponsePhase
  lease : Lease Generation
  usedGenerations : List Generation
  now : Time
  progressSeq : Nat
  continuationRequired : Bool
  tokenChargeRequired : Bool
  continuationCount : Nat
  tokenChargeCount : Nat
  deriving DecidableEq, Repr

def initial (Generation : Type) : World Generation :=
  { request := .pending
  , response := .absent
  , lease := .vacant
  , usedGenerations := []
  , now := 0
  , progressSeq := 0
  , continuationRequired := false
  , tokenChargeRequired := false
  , continuationCount := 0
  , tokenChargeCount := 0
  }

def Outcome.requestPhase : Outcome → RequestPhase
  | .completed => .completed
  | .failed => .failed
  | .interrupted => .interrupted
  | .dead => .dead
  | .superseded => .superseded

def Outcome.responsePhase : Outcome → ResponsePhase
  | .completed => .completed
  | .failed => .failed
  | .interrupted => .interrupted
  | .dead | .superseded => .failed

def terminalAgreement {Generation : Type} (world : World Generation) : Prop :=
  match world.lease with
  | .terminal _ outcome =>
      world.request = outcome.requestPhase ∧
        world.response = outcome.responsePhase
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

def canFinalize {Generation : Type} (world : World Generation)
    (outcome : Outcome) : Prop :=
  match outcome with
  | .completed =>
      world.request = .processing ∧ world.response = .streaming
  | .failed | .interrupted | .dead | .superseded =>
      (world.request = .claimed ∧ world.response = .absent) ∨
        (world.request = .processing ∧ world.response = .streaming)

instance {Generation : Type} (world : World Generation) (outcome : Outcome) :
    Decidable (canFinalize world outcome) := by
  unfold canFinalize
  cases outcome <;> infer_instance

def commitTerminalEffects {Generation : Type}
    (world : World Generation) : World Generation :=
  { world with
    continuationCount := if world.continuationRequired then 1 else 0
    tokenChargeCount := if world.tokenChargeRequired then 1 else 0 }

def terminalize {Generation : Type}
    (world : World Generation) (generation : Generation)
    (outcome : Outcome) : World Generation :=
  commitTerminalEffects
    { world with
      request := outcome.requestPhase
      response := outcome.responsePhase
      lease := .terminal generation outcome }

/-- EOF is transport observation, not evidence of a completed provider turn. -/
def providerEofIsFailure (sawExplicitFinal : Bool) : Bool := !sawExplicitFinal

end RequestExecutionLease
