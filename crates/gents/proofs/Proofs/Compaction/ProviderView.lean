import Proofs.Compaction.Strip
import Proofs.PromptAssembly.Properties

/-!
# The canonical provider view

Issue #993 named `strip ∘ sanitize = sanitize ∘ strip` as unproven, and
therefore named the obvious production fix — moving the compacted-prefix drop
past sanitization — as unlicensed:

> `strip` rewrites tool-result *content* while `sanitize` drops rows based on
> call/result *pairing*, so stripping first can change which pairs sanitize
> considers orphaned.

It does not, and `strip_sanitize_commute` is why: both sanitize stages branch
only on a row's `MessageKind` constructor and its call ids, and `strip` fixes
both. That settles the question affirmatively and licenses `providerView` — the
single reduction the compaction writer and the request reader both index.
-/

namespace Compaction

open Transcript (MessageRow MessageKind ToolResultKey)
open PromptAssembly (sanitize sanitizeTurn dropOrphanedFrom filterCallsBy resolvedIn callsIn
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
which pairs sanitize considers orphaned. -/
theorem strip_sanitize_commute (msgs : List MessageRow) :
    strip (sanitize msgs) = sanitize (strip msgs) := by
  have hres : resolvedIn (dropOrphanedFrom ∅ (strip msgs))
      = resolvedIn (dropOrphanedFrom ∅ msgs) := by
    rw [← strip_dropOrphanedFrom, resolvedIn_strip]
  unfold PromptAssembly.sanitize PromptAssembly.dropUnpairedCalls
    PromptAssembly.dropOrphanedResults
  rw [strip_filterCallsBy, strip_dropOrphanedFrom, hres]

/-- The single canonical narrowing from the durable transcript to the provider
view. Both sides of compaction's prefix accounting index *this* list: the
compaction writer records `messages_compacted` against it, and the request
reader drops that many rows from it. Rust: `compaction::provider_view`. -/
def providerView (msgs : List MessageRow) : List MessageRow := sanitize (strip msgs)

/-- The provider view production actually computes.

`drop_unpaired_tool_calls` scopes resolution to the active turn
(`resolved_keys_per_turn`), so a later turn reusing a call id cannot resurrect
an earlier unpaired announcement. `providerViewTurn_eq_providerView` shows this
is the same list as `providerView` whenever `UniqueCallIds` holds — which is the
hypothesis every theorem below already carries — so the accounting results
transfer verbatim while the model now names what the runtime does. -/
def providerViewTurn (msgs : List MessageRow) : List MessageRow :=
  sanitizeTurn (strip msgs)

theorem providerViewTurn_eq_providerView {msgs : List MessageRow}
    (huniq : UniqueCallIds msgs) : providerViewTurn msgs = providerView msgs :=
  PromptAssembly.sanitizeTurn_eq_sanitize (strip_preserves_uniqueCallIds huniq)

theorem providerView_sound {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    ProviderValid (providerView msgs) :=
  PromptAssembly.sanitize_sound (strip_preserves_uniqueCallIds huniq)

theorem providerViewTurn_sound {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    ProviderValid (providerViewTurn msgs) := by
  rw [providerViewTurn_eq_providerView huniq]
  exact providerView_sound huniq

/-- What lets `compact()` re-normalize its own input for free, so
`messages_compacted` indexes the canonical space whoever the caller is. -/
theorem providerView_idempotent {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    providerView (providerView msgs) = providerView msgs := by
  unfold providerView
  rw [strip_sanitize_commute, strip_idempotent]
  exact PromptAssembly.sanitize_idempotent (strip_preserves_uniqueCallIds huniq)

theorem providerView_nonempty_announcements (msgs : List MessageRow) :
    PromptAssembly.NonemptyAnnouncements (providerView msgs) :=
  PromptAssembly.nonemptyAnnouncements_sanitize _

/-- Stripping commutes with the whole reduction, not just its stages. -/
theorem strip_providerView (msgs : List MessageRow) :
    strip (providerView msgs) = providerView msgs := by
  unfold providerView
  rw [strip_sanitize_commute, strip_idempotent]

end Compaction
