import Proofs.Request.State
import Proofs.CanonicalOutput.State

/-!
# Request execution lease state

The generation parameter is deliberately abstract. The owner may compare
generations for equality and remember used values, but cannot derive a successor.
Claims, recovery, and revocation therefore require a fresh opaque value.

`OutputFact` is a lease-facing observation of immutable canonical output. Its
eligibility is an input from the canonical projection at the authoritative read,
not durable metadata and not a theorem of this machine. Only a validated fact
for this physical request can renew the request lease. Reclassification after
closure or conflict discovery and the composed cross-replica argument remain
outside this owner.
-/

namespace RequestExecutionLease

inductive Outcome where
  | completed
  | failed
  | interrupted
  | dead
  | superseded
  deriving DecidableEq, Repr

/-- The local mutation gate is the only authority represented by this machine.
An observing replica can supply hints, but cannot admit writes or recover work. -/
inductive Boundary where
  | mutationWriteGate
  | observingReplica
  deriving DecidableEq, Repr

/-- Why a visible immutable output record is or is not lease progress. -/
inductive OutputEligibility where
  | currentRequest
  | foreignRequest
  | fork
  | toolOwned
  | beyondExtent
  | malformed
  /-- An authoritative conflict in the current request projection. Conflicts
  outside the request must not be classified with this constructor. -/
  | currentRequestConflict
  deriving DecidableEq, Repr

def OutputEligibility.renewsLease : OutputEligibility → Bool
  | .currentRequest => true
  | _ => false

def OutputEligibility.isConflict : OutputEligibility → Bool
  | .currentRequestConflict => true
  | _ => false

structure OutputFact (Generation : Type) where
  id : CanonicalOutput.DocId
  generation : Generation
  createdAt : Time
  eligibility : OutputEligibility
  deriving DecidableEq, Repr

inductive Lease (Generation : Type) where
  | vacant
  | active (generation : Generation) (duration explicitDeadline : Time)
  /-- Explicitly dropped work only. Expiry recovery swaps generation atomically
  from `active`; it never commits this intermediate state. -/
  | recoverable (generation : Generation) (duration explicitDeadline : Time)
  | terminal (generation : Generation) (outcome : Outcome)
  deriving DecidableEq, Repr

/-- These counters expose duplicate terminal side effects as values above one. -/
structure World (Generation : Type) where
  request : RequestState
  lease : Lease Generation
  usedGenerations : List Generation
  /-- A recomputed authoritative projection snapshot. The lease model does not
  prove the canonical source/extent classifier that supplies this list. -/
  output : List (OutputFact Generation)
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
  , output := []
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

def progressDeadline {Generation : Type} [DecidableEq Generation]
    (generation : Generation) (duration : Time) : List (OutputFact Generation) → Time
  | [] => 0
  | fact :: rest =>
      max
        (if fact.generation = generation ∧ fact.eligibility.renewsLease then
          fact.createdAt + duration
        else 0)
        (progressDeadline generation duration rest)

def eligibleFor {Generation : Type} [DecidableEq Generation]
    (generation : Generation) (fact : OutputFact Generation) : Bool :=
  fact.generation == generation && fact.eligibility.renewsLease

def effectiveExpiry {Generation : Type} [DecidableEq Generation]
    (world : World Generation) : Time :=
  match world.lease with
  | .active generation duration explicitDeadline
  | .recoverable generation duration explicitDeadline =>
      max explicitDeadline (progressDeadline generation duration world.output)
  | .vacant | .terminal _ _ => 0

/-- An authoritative conflict is an integrity failure, not evidence of inactivity. -/
def integrityHealthy {Generation : Type} (world : World Generation) : Prop :=
  world.output.all (fun fact => !fact.eligibility.isConflict) = true

instance {Generation : Type} (world : World Generation) :
    Decidable (integrityHealthy world) := by
  unfold integrityHealthy
  infer_instance

/-- `now` is the owning runtime's monotonic clock. Admitted facts cannot appear
to come from its future; a wall-clock discontinuity therefore blocks decisions
instead of manufacturing or discarding lease time. -/
def clockCoherent {Generation : Type} [DecidableEq Generation]
    (world : World Generation) (generation : Generation) : Prop :=
  world.output.all (fun fact =>
    if eligibleFor generation fact then fact.createdAt ≤ world.now else true) = true

instance {Generation : Type} [DecidableEq Generation]
    (world : World Generation) (generation : Generation) :
    Decidable (clockCoherent world generation) := by
  unfold clockCoherent
  infer_instance

def admitted {Generation : Type} [DecidableEq Generation]
    (world : World Generation) (boundary : Boundary) (generation : Generation) : Prop :=
  boundary = .mutationWriteGate ∧ integrityHealthy world ∧
    match world.lease with
    | .active owner _ _ => owner = generation ∧ clockCoherent world owner ∧
        world.now < effectiveExpiry world
    | _ => False

instance {Generation : Type} [DecidableEq Generation]
    (world : World Generation) (boundary : Boundary) (generation : Generation) :
    Decidable (admitted world boundary generation) := by
  unfold admitted
  cases boundary <;> cases hlease : world.lease <;> simp <;> infer_instance

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
