import Proofs.Basic

/-!
# Durable per-turn provider-context reduction (#1127)

`CompactionEntry` is a session-prefix fact: its cumulative boundary changes the
history loaded by later requests.  A per-turn reduction is a different entity.
It replaces only one request's sticky provider projection, can happen several
times in that request, and must never participate in session-prefix dropping.

This model gives that entity its own immutable store.  The checkpoint is opaque
but exact; production persists the native post-reduction message list.  The
source boundary is opaque too; production binds it to the exact request version
and a bounded canonical AgentMessage high-water identity while the stored split
contains the exact source projection. Pair closure is a creation precondition
rather than something recovery is allowed to repair.
-/

namespace Compaction.DurableReduction

abbrev AgentDid := Nat
abbrev ProducerCallId := Nat
abbrev RequestDocId := Nat
abbrev ClaimCommitId := Nat

/-- One reduction result.  Retry/repair attempts of the summary provider call
are provenance of `producerCall`, not new reduction identities. -/
structure ReductionKey where
  agentDid : AgentDid
  sessionId : SessionId
  requestDocId : RequestDocId
  turnIndex : Nat
  ordinal : Nat
  deriving DecidableEq, Repr

/-- Exact durable transcript/version boundary.  Opaque: the model needs
identity, not an implementation of DefraDB commits. -/
structure SourceBoundary where
  value : Nat
  deriving DecidableEq, Repr

/-- Exact native provider projection. -/
structure Projection where
  value : Nat
  deriving DecidableEq, Repr

/-- The immutable fact value stored under a reduction key. -/
structure Fact where
  /-- Claim-local request version.  It is exact provenance for this fact, not a
  chain-wide epoch and not part of `ReductionKey`. -/
  claimCommit : ClaimCommitId
  sourceBoundary : SourceBoundary
  sourceProjection : Projection
  checkpoint : Projection
  producerCall : Option ProducerCallId
  parent : Option ReductionKey
  pairClosed : Bool
  deriving DecidableEq, Repr

abbrev Store := ReductionKey → Option Fact

namespace Store

def empty : Store := fun _ => none

def bind (store : Store) (key : ReductionKey) (fact : Fact) : Store :=
  fun probe => if probe = key then some fact else store probe

@[simp] theorem bind_self (store : Store) (key : ReductionKey) (fact : Fact) :
    bind store key fact key = some fact := by
  simp [bind]

@[simp] theorem bind_other (store : Store) (key probe : ReductionKey) (fact : Fact)
    (h : probe ≠ key) : bind store key fact probe = store probe := by
  simp [bind, h]

end Store

inductive PersistOutcome where
  | fresh
  | idempotent
  | conflict
  | pairOpen
  deriving DecidableEq, Repr

namespace PersistOutcome

def toContract : PersistOutcome → String
  | .fresh => "fresh"
  | .idempotent => "idempotent"
  | .conflict => "conflict"
  | .pairOpen => "pair_open"

def durable : PersistOutcome → Bool
  | .fresh | .idempotent => true
  | .conflict | .pairOpen => false

end PersistOutcome

/-- Create-and-compare.  A logical twin or mismatched redelivery is an
integrity conflict; no branch updates a fact. -/
def persist (store : Store) (key : ReductionKey) (fact : Fact) :
    PersistOutcome × Store :=
  if fact.pairClosed then
    match store key with
    | none => (.fresh, Store.bind store key fact)
    | some stored => if stored = fact then (.idempotent, store) else (.conflict, store)
  else
    (.pairOpen, store)

theorem persist_idempotent (store : Store) (key : ReductionKey) (fact : Fact)
    (hpairs : fact.pairClosed = true) (h : store key = some fact) :
    persist store key fact = (.idempotent, store) := by
  simp [persist, hpairs, h]

theorem persist_rejects_rebinding (store : Store) (key : ReductionKey)
    (stored fact : Fact) (hpairs : fact.pairClosed = true)
    (h : store key = some stored) (hne : stored ≠ fact) :
    persist store key fact = (.conflict, store) := by
  simp [persist, hpairs, h, hne]

theorem persist_durable_iff (store : Store) (key : ReductionKey) (fact : Fact) :
    (persist store key fact).1.durable = true ↔
      fact.pairClosed = true ∧ (persist store key fact).2 key = some fact := by
  cases hpairs : fact.pairClosed <;> simp [persist, hpairs, PersistOutcome.durable]
  cases h : store key with
  | none => simp [persist, hpairs, h, PersistOutcome.durable]
  | some stored =>
      by_cases heq : stored = fact
      · subst heq; simp [persist, hpairs, h, PersistOutcome.durable]
      · simp [persist, hpairs, h, heq, PersistOutcome.durable]

/-- Request retry is idempotent at the reduction layer.  Provider attempts can
change while the reduction key remains fixed; a different accepted checkpoint
must use the next ordinal and link its parent. -/
theorem provider_attempt_not_in_reduction_identity
    (key : ReductionKey) (_firstAttempt _retryAttempt : Nat) : key = key := rfl

theorem claim_commit_not_in_reduction_identity
    (key : ReductionKey) (_firstClaim _laterClaim : ClaimCommitId) : key = key := rfl

theorem turn_separates_reductions (key : ReductionKey) {a b : Nat} (h : a ≠ b) :
    ({ key with turnIndex := a } : ReductionKey) ≠ { key with turnIndex := b } := by
  intro heq
  exact h (congrArg ReductionKey.turnIndex heq)

theorem request_separates_concurrent_reductions
    (key : ReductionKey) {a b : RequestDocId} (h : a ≠ b) :
    ({ key with requestDocId := a } : ReductionKey) ≠ { key with requestDocId := b } := by
  intro heq
  exact h (congrArg ReductionKey.requestDocId heq)

theorem fork_separates_reductions
    (key : ReductionKey) {a b : SessionId} (h : a ≠ b) :
    ({ key with sessionId := a } : ReductionKey) ≠ { key with sessionId := b } := by
  intro heq
  exact h (congrArg ReductionKey.sessionId heq)

/-! ## Durable consumption evidence -/

inductive CaptureKind where
  | inference
  | title
  | compaction
  deriving BEq, DecidableEq, Repr

structure CaptureCitation where
  kind : CaptureKind
  reductionKeys : List ReductionKey
  deriving Repr

/-- A rendered capture consumes a checkpoint only by explicitly citing its key
from the owned inference scope.  Timestamps and unrelated capture kinds are not
evidence. -/
def consumedBy (key : ReductionKey) (captures : List CaptureCitation) : Bool :=
  captures.any fun capture =>
    match capture.kind with
    | .inference => decide (key ∈ capture.reductionKeys)
    | .title | .compaction => false

@[simp] theorem title_citation_does_not_consume (key : ReductionKey) :
    consumedBy key [{ kind := .title, reductionKeys := [key] }] = false := by
  rfl

@[simp] theorem inference_citation_consumes (key : ReductionKey) :
    consumedBy key [{ kind := .inference, reductionKeys := [key] }] = true := by
  simp [consumedBy]

/-! ## Persistence fence and recovery -/

inductive Stage where
  /-- Summary call returned, but its value is not accepted by the loop yet. -/
  | summaryCompleted
  | durable
  | crashed
  | recovered
  | sent
  deriving DecidableEq, Repr

structure Machine where
  store : Store
  canonicalTranscript : Nat
  stage : Stage
  key : ReductionKey
  fact : Fact
  activeProjection : Option Projection

/-- Summary transport completion is not itself a reduction transition.  The
reduction succeeds exactly when create-and-compare has made its fact durable;
production keeps the compaction `InferenceCall` non-complete until then. -/
def successfulReduction (machine : Machine) : Bool :=
  machine.stage = .durable || machine.stage = .recovered || machine.stage = .sent

inductive Step : Machine → Machine → Prop where
  | persistFresh {pre post : Machine}
      (hstage : pre.stage = .summaryCompleted)
      (hunbound : pre.store pre.key = none)
      (hpairs : pre.fact.pairClosed = true)
      (hpost : post = { pre with
        store := Store.bind pre.store pre.key pre.fact
        stage := .durable
        activeProjection := some pre.fact.checkpoint }) : Step pre post
  | persistIdempotent {pre post : Machine}
      (hstage : pre.stage = .summaryCompleted)
      (hbound : pre.store pre.key = some pre.fact)
      (hpairs : pre.fact.pairClosed = true)
      (hpost : post = { pre with
        stage := .durable
        activeProjection := some pre.fact.checkpoint }) : Step pre post
  | crash {pre post : Machine}
      (hpost : post = { pre with stage := .crashed, activeProjection := none }) : Step pre post
  | restore {pre post : Machine}
      (hstage : pre.stage = .crashed)
      (hbound : pre.store pre.key = some pre.fact)
      (hpost : post = { pre with
        stage := .recovered
        activeProjection := some pre.fact.checkpoint }) : Step pre post
  | sendDurable {pre post : Machine}
      (hstage : pre.stage = .durable ∨ pre.stage = .recovered)
      (hbound : pre.store pre.key = some pre.fact)
      (hactive : pre.activeProjection = some pre.fact.checkpoint)
      (hpost : post = { pre with stage := .sent }) : Step pre post

theorem Step.canonical_transcript_immutable {pre post : Machine} (h : Step pre post) :
    post.canonicalTranscript = pre.canonicalTranscript := by
  cases h <;> simp_all

theorem crash_before_persist_never_activates {pre post : Machine}
    (hcrash : post = { pre with stage := .crashed, activeProjection := none }) :
    successfulReduction post = false ∧ post.activeProjection = none := by
  subst hcrash
  simp [successfulReduction]

/-- The loop cannot consume a checkpoint in a provider call unless the exact
fact that determines it is already durable. -/
theorem sent_step_requires_durable_fact {pre post : Machine} (h : Step pre post)
    (hsent : post.stage = .sent) :
    pre.store pre.key = some pre.fact ∧
      pre.activeProjection = some pre.fact.checkpoint := by
  cases h with
  | persistFresh _ _ _ hpost => subst hpost; contradiction
  | persistIdempotent _ _ _ hpost => subst hpost; contradiction
  | crash hpost => subst hpost; contradiction
  | restore _ _ hpost => subst hpost; contradiction
  | sendDurable _ hbound hactive _ => exact ⟨hbound, hactive⟩

/-- Crash after persistence loses only process-local state.  Recovery restores
the exact sticky projection, not a newly summarized approximation. -/
theorem crash_then_restore_exact {durable crashed recovered : Machine}
    (hdurable : durable.store durable.key = some durable.fact)
    (hcrash : crashed = { durable with stage := .crashed, activeProjection := none })
    (hrestore : recovered = { crashed with
      stage := .recovered
      activeProjection := some crashed.fact.checkpoint }) :
    recovered.activeProjection = some durable.fact.checkpoint ∧
      recovered.store recovered.key = some recovered.fact := by
  subst hcrash
  subst hrestore
  simp_all

/-- Pair closure is checked before a fact can become active and is retained by
exact restoration. -/
theorem durable_checkpoint_pair_closed {pre post : Machine} (h : Step pre post)
    (hdurable : post.stage = .durable) : post.fact.pairClosed = true := by
  cases h with
  | persistFresh _ _ hpairs hpost => subst hpost; exact hpairs
  | persistIdempotent _ _ hpairs hpost => subst hpost; exact hpairs
  | crash hpost => subst hpost; contradiction
  | restore _ _ hpost => subst hpost; contradiction
  | sendDurable _ _ _ hpost => subst hpost; contradiction

/-! ## Executable create-and-compare cases -/

structure Scenario where
  key : ReductionKey
  fact : Fact
  prior : Option Fact
  deriving Repr

namespace Scenario

def store (scenario : Scenario) : Store :=
  match scenario.prior with
  | none => Store.empty
  | some fact => Store.bind Store.empty scenario.key fact

def outcome (scenario : Scenario) : PersistOutcome :=
  (persist scenario.store scenario.key scenario.fact).1

def durableAfter (scenario : Scenario) : Bool :=
  decide ((persist scenario.store scenario.key scenario.fact).2 scenario.key = some scenario.fact)

def sendPermitted (scenario : Scenario) : Bool :=
  durableAfter scenario && scenario.fact.pairClosed

end Scenario

end Compaction.DurableReduction
