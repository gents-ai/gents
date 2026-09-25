import Proofs.Basic
import Proofs.PromptAssembly.ClaudeMap

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

/-- Exact native provider projection. The opaque value identifies the complete
selected provider body; `taggedRows` associates every input row position with
its physical source and original block coordinates. `none` means association
is unknown, not an empty input. Native admission binds this sidecar to the
same built request body before a full-input rewrite can become durable. -/
structure Projection where
  value : Nat
  taggedRows : Option (List PromptAssembly.ClaudeMap.TaggedReplayRow) := none
  retired : List PromptAssembly.ClaudeMap.ReplayTag := []
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

/-- A canonical observation made at the exact pre-rewrite source boundary.
Native must construct these from physical accepted rows after DID/session and
high-water validation, not from a caller-supplied claimed identity. -/
structure SourceObservation where
  tag : PromptAssembly.ClaudeMap.ReplayTag
  agentDid : AgentDid
  sessionId : SessionId
  sourceBoundary : SourceBoundary
  deriving DecidableEq, Repr

inductive FullInputRewriteError where
  | unknownProjection
  | invalidSidecar
  | unboundSource
  | inventedSource
  | changedAssociation
  | staleFrontier
  deriving DecidableEq, Repr

def FullInputRewriteError.toContract : FullInputRewriteError → String
  | .unknownProjection => "unknown_projection"
  | .invalidSidecar => "invalid_sidecar"
  | .unboundSource => "unbound_source"
  | .inventedSource => "invented_source"
  | .changedAssociation => "changed_association"
  | .staleFrontier => "stale_frontier"

private def taggedSources (rows : List PromptAssembly.ClaudeMap.TaggedReplayRow) :
    List PromptAssembly.ClaudeMap.ReplayTag :=
  rows.filterMap (·.source)

/-- A retained physical source may lose whole blocks, and the owned tool-call
repair may change non-reasoning payload. It may not acquire another header,
reindex surviving blocks, or alter its signed reasoning witness. Exact native
body transformation remains the provider-input owner's obligation. -/
def retainedAssociationValid
    (before : List PromptAssembly.ClaudeMap.TaggedReplayRow)
    (row : PromptAssembly.ClaudeMap.TaggedReplayRow) : Bool :=
  match row.source with
  | none => true
  | some tag =>
      match before.filter (fun prior => prior.source == some tag) with
      | [prior] =>
          row.physicalHeader == prior.physicalHeader &&
          decide (List.Sublist row.blockIndices prior.blockIndices) &&
          (PromptAssembly.ClaudeMap.originalReasoningWitness row).all
            (fun witness => decide (witness ∈
              PromptAssembly.ClaudeMap.originalReasoningWitness prior))
      | _ => false

def retirementForRewrite (prior : List PromptAssembly.ClaudeMap.ReplayTag)
    (rows : List PromptAssembly.ClaudeMap.TaggedReplayRow) :
    List PromptAssembly.ClaudeMap.ReplayTag :=
  prior ++ (taggedSources rows).filter PromptAssembly.ClaudeMap.providerReplayTag

theorem prior_retirement_survives_rewrite
    (prior : List PromptAssembly.ClaudeMap.ReplayTag)
    (rows : List PromptAssembly.ClaudeMap.TaggedReplayRow)
    (tag : PromptAssembly.ClaudeMap.ReplayTag) (h : tag ∈ prior) :
    tag ∈ retirementForRewrite prior rows := by
  exact List.mem_append.mpr (Or.inl h)

theorem pre_rewrite_provider_source_retired
    (prior : List PromptAssembly.ClaudeMap.ReplayTag)
    (rows : List PromptAssembly.ClaudeMap.TaggedReplayRow)
    (tag : PromptAssembly.ClaudeMap.ReplayTag)
    (hsource : tag ∈ taggedSources rows)
    (hprovider : PromptAssembly.ClaudeMap.providerReplayTag tag = true) :
    tag ∈ retirementForRewrite prior rows := by
  exact List.mem_append.mpr (Or.inr (List.mem_filter.mpr ⟨hsource, hprovider⟩))

/-- An effective full-input rewrite retires every provider source in its
pre-rewrite projection, regardless of where that row sat in the retained
tail. This is the durable rewrite frontier. Prefix compatibility is an
additional test for later completed turns, never a way to resurrect an old
source after a summary, repair, or signature-rejection rewrite. -/
def rewriteFullInput (key : ReductionKey) (boundary : SourceBoundary)
    (observations : List SourceObservation)
    (source post : Projection) : Except FullInputRewriteError Projection := do
  let some before := source.taggedRows | throw .unknownProjection
  let some after := post.taggedRows | throw .unknownProjection
  if !(before.all PromptAssembly.ClaudeMap.replayRowIndicesValid) ||
      !(after.all PromptAssembly.ClaudeMap.replayRowIndicesValid) ||
      (taggedSources before).length != (taggedSources before).eraseDups.length ||
      (taggedSources after).length != (taggedSources after).eraseDups.length then
    throw .invalidSidecar
  let beforeSources := taggedSources before
  if !(beforeSources.all fun tag =>
      (observations.filter fun observation => observation.tag == tag &&
        observation.agentDid == key.agentDid &&
        observation.sessionId == key.sessionId &&
        observation.sourceBoundary == boundary).length == 1) then
    throw .unboundSource
  if !(taggedSources after).all (fun tag => decide (tag ∈ beforeSources)) then
    throw .inventedSource
  if !(after.all (retainedAssociationValid before)) then
    throw .changedAssociation
  return { post with retired := retirementForRewrite source.retired before }

theorem rewriteFullInput_exact_retirement (key : ReductionKey)
    (boundary : SourceBoundary) (observations : List SourceObservation)
    (source post result : Projection)
    (h : rewriteFullInput key boundary observations source post = .ok result) :
    ∃ before, source.taggedRows = some before ∧
      result.retired = retirementForRewrite source.retired before := by
  unfold rewriteFullInput at h
  cases hbefore : source.taggedRows with
  | none => simp [hbefore] at h
  | some before =>
      cases hafter : post.taggedRows with
      | none => simp [hbefore, hafter] at h
      | some after =>
          simp only [hbefore, hafter, bind_pure_comp, Except.bind] at h
          split at h <;> try contradiction
          split at h <;> try contradiction
          split at h <;> try contradiction
          split at h <;> try contradiction
          cases h
          exact ⟨before, by simp [hbefore], rfl⟩

/-- The existing immutable Fact owns both exact projections. This preparation
changes only its postprojection; the ordinary `persist` owner still decides
fresh/idempotent/conflict and rejects open tool pairs. Session compaction must
commit its cursor in the same transaction as the prepared Fact. -/
def prepareFullInputFact (key : ReductionKey) (observations : List SourceObservation)
    (fact : Fact) : Except FullInputRewriteError Fact := do
  let checkpoint ← rewriteFullInput key fact.sourceBoundary observations
    fact.sourceProjection fact.checkpoint
  return { fact with checkpoint }

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

/-- Historical retirement is read from every immutable fact in the complete,
ordered prior lineage, including facts whose checkpoints have since been
consumed. Consumption only chooses the active provider-input checkpoint.
Native must derive lineage membership from exact DID/session-scoped Fact rows;
omitting a prior key is not a permitted way to drop its retirement. -/
def retiredInLineage (store : Store) (lineage : List ReductionKey) :
    List PromptAssembly.ClaudeMap.ReplayTag :=
  lineage.flatMap fun key =>
    match store key with
    | none => []
    | some fact => fact.checkpoint.retired

def retirementFrontier (store : Store) (lineage : List ReductionKey) :
    List PromptAssembly.ClaudeMap.ReplayTag :=
  (retiredInLineage store lineage).eraseDups

theorem retiredInLineage_append (store : Store) (lineage : List ReductionKey)
    (key : ReductionKey) :
    retiredInLineage store (lineage ++ [key]) =
      retiredInLineage store lineage ++
        (match store key with
         | none => []
         | some fact => fact.checkpoint.retired) := by
  simp [retiredInLineage]

theorem retired_fact_survives_lineage_extension (store : Store)
    (lineage : List ReductionKey) (key : ReductionKey) (fact : Fact)
    (tag : PromptAssembly.ClaudeMap.ReplayTag)
    (hstored : store key = some fact) (hretired : tag ∈ fact.checkpoint.retired) :
    tag ∈ retiredInLineage store (lineage ++ [key]) := by
  rw [retiredInLineage_append, hstored]
  exact List.mem_append.mpr (Or.inr hretired)

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

/-- Full-input rewrites pass through the same immutable create-and-compare
owner after their physical row associations and frontier have been prepared.
This does not by itself couple a session CompactionEntry cursor: the native
transaction must commit that cursor and this Fact in one write gate. -/
def persistFullInput (store : Store) (lineage : List ReductionKey) (key : ReductionKey)
    (observations : List SourceObservation) (fact : Fact) :
    Except FullInputRewriteError (PersistOutcome × Store) := do
  let prepared ← prepareFullInputFact key observations fact
  if (store key).isNone &&
      fact.sourceProjection.retired != retirementFrontier store lineage then
    throw .staleFrontier
  return persist store key prepared

/-- A session CompactionEntry's cursor is paired with the exact immutable
full-input reduction Fact that justified dropping the old provider prefix.
The model treats each pair as one transaction result; native must use one
ConfigAccess transaction for both physical creates and their conflict reads. -/
structure SessionCursorEntry where
  key : ReductionKey
  cursor : Nat
  deriving DecidableEq, Repr

structure SessionRewriteState where
  store : Store
  entries : List SessionCursorEntry

def latestSessionCursor (entries : List SessionCursorEntry) : Nat :=
  (entries.getLast?).map (·.cursor) |>.getD 0

inductive SessionRewriteOutcome where
  | fresh
  | idempotent
  | rewriteRejected (error : FullInputRewriteError)
  | factConflict
  | pairOpen
  | cursorConflict
  deriving DecidableEq, Repr

def SessionRewriteOutcome.toContract : SessionRewriteOutcome → String
  | .fresh => "fresh"
  | .idempotent => "idempotent"
  | .rewriteRejected error => error.toContract
  | .factConflict => "fact_conflict"
  | .pairOpen => "pair_open"
  | .cursorConflict => "cursor_conflict"

/-- Repair-only rewrites use `persistFullInput` directly; a session summary
uses this same Fact owner joined to its CompactionEntry cursor. A repeated
older committed pair remains idempotent after a newer entry, while a lone
fact or lone cursor is an integrity conflict, never a recovery invitation. -/
def commitSessionRewrite (state : SessionRewriteState)
    (lineage : List ReductionKey) (key : ReductionKey)
    (observations : List SourceObservation) (fact : Fact)
    (cursor : Nat) : SessionRewriteOutcome × SessionRewriteState :=
  match persistFullInput state.store lineage key observations fact with
  | .error error => (.rewriteRejected error, state)
  | .ok (outcome, updatedStore) =>
      let entry : SessionCursorEntry := { key, cursor }
      match outcome with
      | .fresh =>
          if cursor > latestSessionCursor state.entries &&
              !(state.entries.any fun existing => existing.key == key) then
            (.fresh, { store := updatedStore, entries := state.entries ++ [entry] })
          else (.cursorConflict, state)
      | .idempotent =>
          if entry ∈ state.entries then (.idempotent, state)
          else (.cursorConflict, state)
      | .conflict => (.factConflict, state)
      | .pairOpen => (.pairOpen, state)

theorem invalid_session_rewrite_preserves_state
    (state : SessionRewriteState) (lineage : List ReductionKey)
    (key : ReductionKey) (observations : List SourceObservation)
    (fact : Fact) (cursor : Nat) (error : FullInputRewriteError)
    (h : persistFullInput state.store lineage key observations fact = .error error) :
    commitSessionRewrite state lineage key observations fact cursor =
      (.rewriteRejected error, state) := by
  simp [commitSessionRewrite, h]

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
  /-- Only a supported provenance format is consumption evidence. -/
  supported : Bool
  reductionKeys : List ReductionKey
  deriving Repr

/-- A rendered capture consumes a checkpoint only by explicitly citing its key
from the owned inference scope.  Timestamps and unrelated capture kinds are not
evidence. -/
def consumedBy (key : ReductionKey) (captures : List CaptureCitation) : Bool :=
  captures.any fun capture =>
    capture.supported &&
      match capture.kind with
      | .inference => decide (key ∈ capture.reductionKeys)
      | .title | .compaction => false

@[simp] theorem title_citation_does_not_consume (key : ReductionKey) :
    consumedBy key [{ kind := .title, supported := true, reductionKeys := [key] }] = false := by
  rfl

@[simp] theorem inference_citation_consumes (key : ReductionKey) :
    consumedBy key [{ kind := .inference, supported := true, reductionKeys := [key] }] = true := by
  simp [consumedBy]

@[simp] theorem unsupported_inference_does_not_consume (key : ReductionKey) :
    consumedBy key
      [{ kind := .inference, supported := false, reductionKeys := [key] }] = false := by
  rfl

/-! ## Active checkpoint versus immutable lineage -/

/-- Every key remains in lineage to order the next fact, but only the newest
checkpoint shapes the sticky provider projection after recovery. -/
def activeKeys : List ReductionKey → List ReductionKey
  | [] => []
  | key :: rest =>
      match rest.getLast? with
      | none => [key]
      | some latest => [latest]

@[simp] theorem activeKeys_singleton (key : ReductionKey) :
    activeKeys [key] = [key] := by
  rfl

theorem activeKeys_append_nonempty (lineage : List ReductionKey) (key : ReductionKey) :
    activeKeys (lineage ++ [key]) = [key] := by
  cases lineage with
  | nil => rfl
  | cons head tail => simp [activeKeys]

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

/-- Recovery trusts facts admitted by the creation owner. Arbitrary external
stores do not acquire pair closure merely by containing a matching key. -/
def Store.PairClosed (store : Store) : Prop :=
  ∀ key fact, store key = some fact → fact.pairClosed = true

@[simp] theorem Store.empty_pairClosed : Store.PairClosed Store.empty := by
  intro key fact h
  simp [Store.empty] at h

theorem Store.bind_pairClosed (store : Store) (key : ReductionKey) (fact : Fact)
    (hstore : store.PairClosed) (hpairs : fact.pairClosed = true) :
    (Store.bind store key fact).PairClosed := by
  intro probe stored h
  by_cases heq : probe = key
  · subst heq
    simp at h
    cases h
    exact hpairs
  · exact hstore probe stored (by simpa [Store.bind, heq] using h)

theorem Step.preserves_pairClosed_store {pre post : Machine}
    (hstore : pre.store.PairClosed) (hstep : Step pre post) : post.store.PairClosed := by
  cases hstep with
  | persistFresh _ _ hpairs hpost =>
      subst hpost
      exact Store.bind_pairClosed _ _ _ hstore hpairs
  | persistIdempotent _ _ _ hpost => subst hpost; exact hstore
  | crash hpost => subst hpost; exact hstore
  | restore _ _ hpost => subst hpost; exact hstore
  | sendDurable _ _ _ hpost => subst hpost; exact hstore

/-- Creation checks pair closure; recovery and send retain it from a valid
store. No recovery repair or additional transition is introduced. -/
theorem active_checkpoint_pair_closed {pre post : Machine}
    (hstore : pre.store.PairClosed) (hstep : Step pre post)
    (hactive : successfulReduction post = true) : post.fact.pairClosed = true := by
  cases hstep with
  | persistFresh _ _ hpairs hpost => subst hpost; exact hpairs
  | persistIdempotent _ _ hpairs hpost => subst hpost; exact hpairs
  | crash hpost => subst hpost; simp [successfulReduction] at hactive
  | restore _ hbound hpost => subst hpost; exact hstore _ _ hbound
  | sendDurable _ hbound _ hpost => subst hpost; exact hstore _ _ hbound

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
