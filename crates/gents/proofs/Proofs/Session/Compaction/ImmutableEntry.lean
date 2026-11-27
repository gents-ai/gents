import Proofs.Shared.Basic
import Proofs.Session.Compaction.Cursor

namespace Compaction.ImmutableEntry

/-- Exact pinned identity and commit version of one source `AgentMessage`
covered by a durable `CompactionEntry`. Once committed, the entry's fields are
all `@immutable` and its covered message manifest cannot be rewritten. -/
structure PinnedSourceMessage where
  docId : Nat
  sequence : Nat
  commitCid : Nat
  deriving DecidableEq, Repr

/-- Strictly increasing sequence order over stored messages within one session
scope. -/
def strictlyIncreasing : List PinnedSourceMessage → Bool
  | [] => true
  | [_] => true
  | first :: second :: rest =>
      decide (first.sequence < second.sequence) && strictlyIncreasing (second :: rest)

/-- The exact ordered slice of stored session messages newly covered by a
compaction entry: strictly after `priorCursor` (when any) and up through
`cursor`. -/
def sliceForRange (priorCursor : Option Nat) (cursor : Nat)
    (stored : List PinnedSourceMessage) : List PinnedSourceMessage :=
  stored.filter fun m =>
    priorCursor.all (· < m.sequence) && decide (m.sequence ≤ cursor)

/-- A compaction entry's pinned source-message manifest is valid iff the cursor
advances past any prior cursor, stored sequences are strictly increasing, the
covered slice is non-empty and ends at `cursor`, and `manifest` equals that
exact slice (pinning every covered `docId`, `sequence`, and `commitCid`). -/
def validSourceManifest (priorCursor : Option Nat) (cursor : Nat)
    (stored manifest : List PinnedSourceMessage) : Bool :=
  priorCursor.all (· < cursor) &&
    strictlyIncreasing stored &&
    let expected := sliceForRange priorCursor cursor stored
    !expected.isEmpty &&
      expected.getLast?.map (·.sequence) == some cursor &&
      manifest == expected

/-- Complete immutable payload of one session `CompactionEntry` fact. -/
structure CompactionFact where
  requestDocId : Nat
  summary : Nat
  messagesCompacted : Nat
  compactedThroughSequence : Nat
  originalTokens : Nat
  compactedTokens : Nat
  filesRead : List Nat
  filesModified : List Nat
  sourceMessages : List PinnedSourceMessage
  deriving DecidableEq, Repr

/-- Decision of the create-only `CompactionEntry` writer for one attempt against
the session's committed compaction chain. -/
inductive WriteOutcome where
  | create
  | idempotentReplay
  | staleGeneration
  | integrityConflict
  | invalidCursor
  | invalidSourceVersions
  deriving DecidableEq, Repr

namespace WriteOutcome

def toContract : WriteOutcome → String
  | .create => "create"
  | .idempotentReplay => "idempotent_replay"
  | .staleGeneration => "stale_generation"
  | .integrityConflict => "integrity_conflict"
  | .invalidCursor => "invalid_cursor"
  | .invalidSourceVersions => "invalid_source_versions"

end WriteOutcome

/-- Create-only idempotency and conflict decision for `CompactionEntry`:
- When the caller observed the current chain tip (`expectedGenerationLen = chain.length`),
  the cursor must strictly advance past any prior cursor, name a stored message,
  and carry the exact pinned source-message manifest for `(priorCursor, cursor]`.
- When another entry already landed at `expectedGenerationLen`:
  - an exact payload + pinned-manifest match is an idempotent replay (even if
    later compaction generations have already landed after it);
  - a retry by the same request with any mismatched field or source version is a
    typed integrity conflict (never downgraded to a benign stale-generation race);
  - a different request that raced from the same predecessor generation sees a
    stale generation and rebuilds from the winner. -/
def decideWrite (chain : List CompactionFact) (expectedGenerationLen : Nat)
    (candidate : CompactionFact) (stored : List PinnedSourceMessage) : WriteOutcome :=
  if expectedGenerationLen == chain.length then
    let priorCursor := chain.getLast?.map (·.compactedThroughSequence)
    if !priorCursor.all (· < candidate.compactedThroughSequence) then
      .invalidCursor
    else if !Compaction.Cursor.namesStoredMessage
        (stored.map (·.sequence)) candidate.compactedThroughSequence then
      .invalidCursor
    else if !validSourceManifest priorCursor candidate.compactedThroughSequence
        stored candidate.sourceMessages then
      .invalidSourceVersions
    else
      .create
  else
    match chain[expectedGenerationLen]? with
    | some persisted =>
        if persisted == candidate then
          .idempotentReplay
        else if persisted.requestDocId == candidate.requestDocId then
          .integrityConflict
        else
          .staleGeneration
    | none => .staleGeneration

theorem validSourceManifest_pins_exact_manifest
    (priorCursor : Option Nat) (cursor : Nat)
    (stored manifest : List PinnedSourceMessage)
    (h : validSourceManifest priorCursor cursor stored manifest = true) :
    manifest = sliceForRange priorCursor cursor stored := by
  simp only [validSourceManifest, Bool.and_eq_true, beq_iff_eq] at h
  exact h.2.2

theorem validSourceManifest_rejects_regression
    (prior cursor : Nat) (stored manifest : List PinnedSourceMessage)
    (hle : cursor ≤ prior) :
    validSourceManifest (some prior) cursor stored manifest = false := by
  have hnot : decide (prior < cursor) = false := by
    simp [Nat.not_lt.mpr hle]
  simp [validSourceManifest, Option.all, hnot]

theorem validSourceManifest_implies_cursor_named
    (priorCursor : Option Nat) (cursor : Nat)
    (stored manifest : List PinnedSourceMessage)
    (h : validSourceManifest priorCursor cursor stored manifest = true) :
    Compaction.Cursor.namesStoredMessage (stored.map (·.sequence)) cursor = true := by
  simp only [validSourceManifest, Bool.and_eq_true, beq_iff_eq] at h
  have hlast := h.2.1.2
  cases hget : (sliceForRange priorCursor cursor stored).getLast? with
  | none => simp [hget] at hlast
  | some m =>
      rw [hget] at hlast
      simp only [Option.map_some', Option.some.injEq] at hlast
      have hmemSlice : m ∈ sliceForRange priorCursor cursor stored :=
        List.mem_of_getLast? hget
      have hmemStored : m ∈ stored := (List.mem_filter.mp hmemSlice).1
      rw [Compaction.Cursor.namesStoredMessage_iff]
      exact List.mem_map.mpr ⟨m, hmemStored, hlast⟩

theorem create_requires_valid_source_manifest
    (chain : List CompactionFact) (expectedGenerationLen : Nat)
    (candidate : CompactionFact) (stored : List PinnedSourceMessage)
    (h : decideWrite chain expectedGenerationLen candidate stored = .create) :
    validSourceManifest (chain.getLast?.map (·.compactedThroughSequence))
      candidate.compactedThroughSequence stored candidate.sourceMessages = true := by
  unfold decideWrite at h
  split at h
  · dsimp only at h
    split at h
    · contradiction
    · split at h
      · contradiction
      · split at h
        · contradiction
        · rename_i _ _ _ hmanifest
          simpa [Bool.not_eq_true] using hmanifest
  · split at h
    · split at h
      · contradiction
      · split at h <;> contradiction
    · contradiction

private theorem lt_length_of_getElem?_eq_some {α : Type} {xs : List α} {i : Nat} {a : α}
    (h : xs[i]? = some a) : i < xs.length := by
  match Nat.lt_or_ge i xs.length with
  | Or.inl hlt => exact hlt
  | Or.inr hge =>
      rw [List.getElem?_eq_none hge] at h
      contradiction

theorem exact_redelivery_is_idempotent
    (chain : List CompactionFact) (expectedGenerationLen : Nat)
    (persisted : CompactionFact) (stored : List PinnedSourceMessage)
    (hbound : chain[expectedGenerationLen]? = some persisted) :
    decideWrite chain expectedGenerationLen persisted stored = .idempotentReplay := by
  have hlt : expectedGenerationLen < chain.length := lt_length_of_getElem?_eq_some hbound
  have hne : (expectedGenerationLen == chain.length) = false := by
    simp [Nat.ne_of_lt hlt]
  simp [decideWrite, hne, hbound]

theorem mismatched_same_request_retry_is_integrity_conflict
    (chain : List CompactionFact) (expectedGenerationLen : Nat)
    (persisted candidate : CompactionFact) (stored : List PinnedSourceMessage)
    (hbound : chain[expectedGenerationLen]? = some persisted)
    (hsame : persisted.requestDocId = candidate.requestDocId)
    (hdiff : persisted ≠ candidate) :
    decideWrite chain expectedGenerationLen candidate stored = .integrityConflict := by
  have hlt : expectedGenerationLen < chain.length := lt_length_of_getElem?_eq_some hbound
  have hne : (expectedGenerationLen == chain.length) = false := by
    simp [Nat.ne_of_lt hlt]
  have hneq : (persisted == candidate) = false := by
    simp [beq_eq_false_iff_ne, hdiff]
  simp [decideWrite, hne, hbound, hneq, hsame]

theorem mismatched_source_versions_retry_is_integrity_conflict
    (chain : List CompactionFact) (expectedGenerationLen : Nat)
    (persisted candidate : CompactionFact) (stored : List PinnedSourceMessage)
    (hbound : chain[expectedGenerationLen]? = some persisted)
    (hsame : persisted.requestDocId = candidate.requestDocId)
    (hversions : persisted.sourceMessages ≠ candidate.sourceMessages) :
    decideWrite chain expectedGenerationLen candidate stored = .integrityConflict := by
  apply mismatched_same_request_retry_is_integrity_conflict chain expectedGenerationLen
    persisted candidate stored hbound hsame
  intro heq
  exact hversions (congrArg CompactionFact.sourceMessages heq)

theorem different_request_race_is_stale_generation
    (chain : List CompactionFact) (expectedGenerationLen : Nat)
    (persisted candidate : CompactionFact) (stored : List PinnedSourceMessage)
    (hbound : chain[expectedGenerationLen]? = some persisted)
    (hreq : persisted.requestDocId ≠ candidate.requestDocId) :
    decideWrite chain expectedGenerationLen candidate stored = .staleGeneration := by
  have hlt : expectedGenerationLen < chain.length := lt_length_of_getElem?_eq_some hbound
  have hne : (expectedGenerationLen == chain.length) = false := by
    simp [Nat.ne_of_lt hlt]
  have hdiff : persisted ≠ candidate := fun heq =>
    hreq (congrArg CompactionFact.requestDocId heq)
  have hneq : (persisted == candidate) = false := by
    simp [beq_eq_false_iff_ne, hdiff]
  have hreqFalse : (persisted.requestDocId == candidate.requestDocId) = false := by
    simp [beq_eq_false_iff_ne, hreq]
  simp [decideWrite, hne, hbound, hneq, hreqFalse]

theorem cursor_regression_is_invalid
    (chain : List CompactionFact) (last candidate : CompactionFact)
    (stored : List PinnedSourceMessage)
    (hlast : chain.getLast? = some last)
    (hle : candidate.compactedThroughSequence ≤ last.compactedThroughSequence) :
    decideWrite chain chain.length candidate stored = .invalidCursor := by
  have hnot : decide (last.compactedThroughSequence < candidate.compactedThroughSequence) = false := by
    simp [Nat.not_lt.mpr hle]
  simp [decideWrite, hlast, Option.all, hnot]

theorem unmatched_cursor_is_invalid
    (chain : List CompactionFact) (candidate : CompactionFact)
    (stored : List PinnedSourceMessage)
    (hadv : (chain.getLast?.map (·.compactedThroughSequence)).all
      (· < candidate.compactedThroughSequence) = true)
    (hmissing : Compaction.Cursor.namesStoredMessage
      (stored.map (·.sequence)) candidate.compactedThroughSequence = false) :
    decideWrite chain chain.length candidate stored = .invalidCursor := by
  simp [decideWrite, hadv, hmissing]

end Compaction.ImmutableEntry
