import Proofs.Compaction.Strip
import Proofs.PromptAssembly.Properties

/-!
# Compaction row projections

`providerViewTurn` composes strip with the per-turn row sanitizer. The retained
`providerViewGlobal` is a unique-call-ID proof model used by older prefix and
cursor lemmas. Their equality requires `UniqueCallIds`; do not implement the
global algorithm from those lemmas. Neither row projection models assistant
prose surviving an unresolved call: that content boundary is modeled separately
in `PromptAssembly.Provider`, currently also under global-resolution assumptions.
-/

namespace Compaction

open Transcript (MessageRow MessageKind ToolResultKey)
open PromptAssembly (sanitizeGlobal sanitizeTurn dropOrphanedFrom filterCallsBy resolvedIn callsIn
                     withKind UniqueCallIds ProviderValid)

theorem stripRow_withKind_assistant (row : MessageRow)
    (callIds : Finset ToolExecution.ToolCallId) :
    stripRow (withKind row (.assistantToolCalls callIds))
      = withKind (stripRow row) (.assistantToolCalls callIds) := rfl

theorem strip_dropOrphanedFrom (l : List MessageRow) :
    ∀ pending, strip (dropOrphanedFrom pending l) = dropOrphanedFrom pending (strip l) := by
  induction l with
  | nil => intro _; rfl
  | cons row rest ih =>
      intro pending
      cases hk : row.kind with
      | toolResult callId key =>
          rw [strip_cons,
            PromptAssembly.dropOrphanedFrom_cons_result (stripRow row) (strip rest) pending
              callId (stubKey key) (strip_kind_result row callId key hk),
            PromptAssembly.dropOrphanedFrom_cons_result row rest pending callId key hk]
          by_cases hmem : callId ∈ pending
          · rw [if_pos hmem, if_pos hmem, strip_cons, ih (pending.erase callId)]
          · rw [if_neg hmem, if_neg hmem, ih pending]
      | assistantToolCalls callIds =>
          rw [strip_cons,
            PromptAssembly.dropOrphanedFrom_cons_assistant (stripRow row) (strip rest) pending
              callIds (strip_kind_assistant row callIds hk),
            PromptAssembly.dropOrphanedFrom_cons_assistant row rest pending callIds hk,
            strip_cons, ih callIds]
      | ordinary =>
          rw [strip_cons,
            PromptAssembly.dropOrphanedFrom_cons_ordinary (stripRow row) (strip rest) pending
              (strip_kind_ordinary row hk),
            PromptAssembly.dropOrphanedFrom_cons_ordinary row rest pending hk,
            strip_cons, ih ∅]

theorem strip_filterCallsBy (l : List MessageRow) :
    ∀ resolved, strip (filterCallsBy resolved l) = filterCallsBy resolved (strip l) := by
  induction l with
  | nil => intro _; rfl
  | cons row rest ih =>
      intro resolved
      cases hk : row.kind with
      | assistantToolCalls callIds =>
          rw [strip_cons,
            PromptAssembly.filterCallsBy_cons_assistant (stripRow row) (strip rest) resolved
              callIds (strip_kind_assistant row callIds hk),
            PromptAssembly.filterCallsBy_cons_assistant row rest resolved callIds hk]
          by_cases hempty : callIds ∩ resolved = ∅
          · rw [if_pos hempty, if_pos hempty, ih resolved]
          · rw [if_neg hempty, if_neg hempty, strip_cons, ih resolved,
              stripRow_withKind_assistant]
      | toolResult callId key =>
          rw [strip_cons,
            PromptAssembly.filterCallsBy_cons_result (stripRow row) (strip rest) resolved
              callId (stubKey key) (strip_kind_result row callId key hk),
            PromptAssembly.filterCallsBy_cons_result row rest resolved callId key hk,
            strip_cons, ih resolved]
      | ordinary =>
          rw [strip_cons,
            PromptAssembly.filterCallsBy_cons_ordinary (stripRow row) (strip rest) resolved
              (strip_kind_ordinary row hk),
            PromptAssembly.filterCallsBy_cons_ordinary row rest resolved hk,
            strip_cons, ih resolved]

/-- The theorem #993 named as the blocker. Stripping first does *not* change
which pairs sanitizeGlobal considers orphaned. -/
theorem strip_sanitize_commute (msgs : List MessageRow) :
    strip (sanitizeGlobal msgs) = sanitizeGlobal (strip msgs) := by
  have hres : resolvedIn (dropOrphanedFrom ∅ (strip msgs))
      = resolvedIn (dropOrphanedFrom ∅ msgs) := by
    rw [← strip_dropOrphanedFrom, resolvedIn_strip]
  unfold PromptAssembly.sanitizeGlobal PromptAssembly.dropUnpairedCalls
    PromptAssembly.dropOrphanedResults
  rw [strip_filterCallsBy, strip_dropOrphanedFrom, hres]

/-- Global-resolution row projection retained for conditional prefix proofs.
This is not the production sanitizer. -/
def providerViewGlobal (msgs : List MessageRow) : List MessageRow := sanitizeGlobal (strip msgs)

/-- Per-turn narrowing of the row-only transcript abstraction.

`drop_unpaired_tool_calls` scopes resolution to the active turn
(`resolved_keys_per_turn`), so a later turn reusing a call id cannot resurrect
an earlier unpaired announcement. `providerViewTurn_eq_providerViewGlobal` shows this
is the same list as `providerViewGlobal` when `UniqueCallIds` holds. Applying
global accounting lemmas to this view must discharge that premise explicitly. -/
def providerViewTurn (msgs : List MessageRow) : List MessageRow :=
  sanitizeTurn (strip msgs)

theorem providerViewTurn_eq_providerViewGlobal {msgs : List MessageRow}
    (huniq : UniqueCallIds msgs) : providerViewTurn msgs = providerViewGlobal msgs :=
  PromptAssembly.sanitizeTurn_eq_sanitizeGlobal (strip_preserves_uniqueCallIds huniq)

theorem providerViewGlobal_sound {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    ProviderValid (providerViewGlobal msgs) :=
  PromptAssembly.sanitizeGlobal_sound (strip_preserves_uniqueCallIds huniq)

theorem providerViewTurn_sound {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    ProviderValid (providerViewTurn msgs) := by
  rw [providerViewTurn_eq_providerViewGlobal huniq]
  exact providerViewGlobal_sound huniq

/-- What lets `compact()` re-normalize its own input for free, so
`messages_compacted` indexes the canonical space whoever the caller is. -/
theorem providerViewGlobal_idempotent {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    providerViewGlobal (providerViewGlobal msgs) = providerViewGlobal msgs := by
  unfold providerViewGlobal
  rw [strip_sanitize_commute, strip_idempotent]
  exact PromptAssembly.sanitizeGlobal_idempotent (strip_preserves_uniqueCallIds huniq)

theorem providerViewGlobal_nonempty_announcements (msgs : List MessageRow) :
    PromptAssembly.NonemptyAnnouncements (providerViewGlobal msgs) :=
  PromptAssembly.nonemptyAnnouncements_sanitize _

/-- Stripping commutes with the whole reduction, not just its stages. -/
theorem strip_providerView (msgs : List MessageRow) :
    strip (providerViewGlobal msgs) = providerViewGlobal msgs := by
  unfold providerViewGlobal
  rw [strip_sanitize_commute, strip_idempotent]

end Compaction
