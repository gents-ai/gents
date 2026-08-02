import Proofs.Compaction.ProviderView

/-!
# The compacted-prefix index correspondence

Defect 3 of #993: `messages_compacted` was measured against
`strip(sanitize(strip H))` but applied to `strip H`. Whenever `sanitize` removed
anything at or before the boundary the two indexings diverged — either
summarized messages survived verbatim alongside their own summary, or messages
that were never summarized were silently dropped from the provider view.

Measuring and dropping in one space is only correct if that space is stable
under the transcript growing. `providerView_append` is that obligation: the
provider view of a longer history begins with the provider view of the shorter
one, so a count recorded against `providerView H` still names the same rows in
`providerView (H ++ new)`.

The hypothesis is exactly "the suffix contributes no tool result for a call
announced in the prefix", with two checkable sufficient conditions below. The
second is what production satisfies: a new request appends its user prompt — an
ordinary row — before anything else.
-/

namespace Compaction

open Transcript (MessageRow MessageKind ToolResultKey)
open PromptAssembly (sanitize dropOrphanedFrom filterCallsBy resolvedIn callsIn
                     UniqueCallIds ProviderValid ActiveBlockValid ActiveBlockValidFrom)

/-- The pending-call set `dropOrphanedFrom` threads: an assistant announcement
replaces it, a matching tool result erases one id, anything else clears it.
Rust mirrors this in `compaction::history::pair_safe_boundary`. -/
def pendingAfter (pending : Finset ToolExecution.ToolCallId) :
    List MessageRow → Finset ToolExecution.ToolCallId
  | [] => pending
  | row :: rest =>
    match row.kind with
    | .toolResult callId _ =>
        if callId ∈ pending then pendingAfter (pending.erase callId) rest
        else pendingAfter pending rest
    | .assistantToolCalls callIds => pendingAfter callIds rest
    | .ordinary => pendingAfter ∅ rest

@[simp] theorem pendingAfter_nil (pending : Finset ToolExecution.ToolCallId) :
    pendingAfter pending [] = pending := rfl

theorem pendingAfter_cons_result (row : MessageRow) (rest : List MessageRow)
    (pending : Finset ToolExecution.ToolCallId) (callId : ToolExecution.ToolCallId)
    (key : ToolResultKey) (h : row.kind = .toolResult callId key) :
    pendingAfter pending (row :: rest) =
      if callId ∈ pending then pendingAfter (pending.erase callId) rest
      else pendingAfter pending rest := by
  simp only [pendingAfter, h]

theorem pendingAfter_cons_assistant (row : MessageRow) (rest : List MessageRow)
    (pending callIds : Finset ToolExecution.ToolCallId)
    (h : row.kind = .assistantToolCalls callIds) :
    pendingAfter pending (row :: rest) = pendingAfter callIds rest := by
  simp only [pendingAfter, h]

theorem pendingAfter_cons_ordinary (row : MessageRow) (rest : List MessageRow)
    (pending : Finset ToolExecution.ToolCallId) (h : row.kind = .ordinary) :
    pendingAfter pending (row :: rest) = pendingAfter ∅ rest := by
  simp only [pendingAfter, h]

theorem pendingAfter_strip (l : List MessageRow) :
    ∀ pending, pendingAfter pending (strip l) = pendingAfter pending l := by
  induction l with
  | nil => intro _; rfl
  | cons row rest ih =>
      intro pending
      rw [strip_cons]
      cases hk : row.kind with
      | toolResult callId key =>
          rw [pendingAfter_cons_result (stripRow row) (strip rest) pending callId (stubKey key)
              (strip_kind_result row callId key hk),
            pendingAfter_cons_result row rest pending callId key hk]
          by_cases hmem : callId ∈ pending
          · rw [if_pos hmem, if_pos hmem, ih (pending.erase callId)]
          · rw [if_neg hmem, if_neg hmem, ih pending]
      | assistantToolCalls callIds =>
          rw [pendingAfter_cons_assistant (stripRow row) (strip rest) pending callIds
              (strip_kind_assistant row callIds hk),
            pendingAfter_cons_assistant row rest pending callIds hk, ih callIds]
      | ordinary =>
          rw [pendingAfter_cons_ordinary (stripRow row) (strip rest) pending
              (strip_kind_ordinary row hk),
            pendingAfter_cons_ordinary row rest pending hk, ih ∅]

theorem resolvedIn_append (a b : List MessageRow) :
    resolvedIn (a ++ b) = resolvedIn a ∪ resolvedIn b := by
  induction a with
  | nil => simp
  | cons row rest ih =>
      cases hk : row.kind with
      | assistantToolCalls callIds =>
          rw [List.cons_append, PromptAssembly.resolvedIn_cons_assistant row _ callIds hk,
            PromptAssembly.resolvedIn_cons_assistant row rest callIds hk, ih]
      | toolResult callId key =>
          rw [List.cons_append, PromptAssembly.resolvedIn_cons_result row _ callId key hk,
            PromptAssembly.resolvedIn_cons_result row rest callId key hk, ih,
            Finset.insert_union]
      | ordinary =>
          rw [List.cons_append, PromptAssembly.resolvedIn_cons_ordinary row _ hk,
            PromptAssembly.resolvedIn_cons_ordinary row rest hk, ih]

theorem dropOrphanedFrom_append (a : List MessageRow) :
    ∀ (b : List MessageRow) (pending : Finset ToolExecution.ToolCallId),
      dropOrphanedFrom pending (a ++ b) =
        dropOrphanedFrom pending a ++ dropOrphanedFrom (pendingAfter pending a) b := by
  induction a with
  | nil => intro b pending; rfl
  | cons row rest ih =>
      intro b pending
      cases hk : row.kind with
      | toolResult callId key =>
          rw [List.cons_append,
            PromptAssembly.dropOrphanedFrom_cons_result row _ pending callId key hk,
            PromptAssembly.dropOrphanedFrom_cons_result row rest pending callId key hk,
            pendingAfter_cons_result row rest pending callId key hk]
          by_cases hmem : callId ∈ pending
          · rw [if_pos hmem, if_pos hmem, if_pos hmem, List.cons_append,
              ih b (pending.erase callId)]
          · rw [if_neg hmem, if_neg hmem, if_neg hmem, ih b pending]
      | assistantToolCalls callIds =>
          rw [List.cons_append,
            PromptAssembly.dropOrphanedFrom_cons_assistant row _ pending callIds hk,
            PromptAssembly.dropOrphanedFrom_cons_assistant row rest pending callIds hk,
            pendingAfter_cons_assistant row rest pending callIds hk, List.cons_append,
            ih b callIds]
      | ordinary =>
          rw [List.cons_append,
            PromptAssembly.dropOrphanedFrom_cons_ordinary row _ pending hk,
            PromptAssembly.dropOrphanedFrom_cons_ordinary row rest pending hk,
            pendingAfter_cons_ordinary row rest pending hk, List.cons_append, ih b ∅]

theorem filterCallsBy_append (a : List MessageRow) :
    ∀ (b : List MessageRow) (resolved : Finset ToolExecution.ToolCallId),
      filterCallsBy resolved (a ++ b) =
        filterCallsBy resolved a ++ filterCallsBy resolved b := by
  induction a with
  | nil => intro b resolved; rfl
  | cons row rest ih =>
      intro b resolved
      cases hk : row.kind with
      | assistantToolCalls callIds =>
          rw [List.cons_append,
            PromptAssembly.filterCallsBy_cons_assistant row _ resolved callIds hk,
            PromptAssembly.filterCallsBy_cons_assistant row rest resolved callIds hk]
          by_cases hempty : callIds ∩ resolved = ∅
          · rw [if_pos hempty, if_pos hempty, ih b resolved]
          · rw [if_neg hempty, if_neg hempty, List.cons_append, ih b resolved]
      | toolResult callId key =>
          rw [List.cons_append,
            PromptAssembly.filterCallsBy_cons_result row _ resolved callId key hk,
            PromptAssembly.filterCallsBy_cons_result row rest resolved callId key hk,
            List.cons_append, ih b resolved]
      | ordinary =>
          rw [List.cons_append,
            PromptAssembly.filterCallsBy_cons_ordinary row _ resolved hk,
            PromptAssembly.filterCallsBy_cons_ordinary row rest resolved hk,
            List.cons_append, ih b resolved]

theorem uniqueCallIds_append_disjoint :
    ∀ {a b : List MessageRow}, UniqueCallIds (a ++ b) → Disjoint (callsIn a) (callsIn b) := by
  intro a
  induction a with
  | nil => intro b _; simp
  | cons row rest ih =>
      intro b h
      cases hk : row.kind with
      | assistantToolCalls callIds =>
          have hsplit := (PromptAssembly.uniqueCallIds_cons_assistant row (rest ++ b) callIds hk).mp h
          have hb : Disjoint callIds (callsIn b) := by
            rw [PromptAssembly.callsIn_append, Finset.disjoint_union_right] at hsplit
            exact hsplit.1.2
          rw [PromptAssembly.callsIn_cons_assistant row rest callIds hk,
            Finset.disjoint_union_left]
          exact ⟨hb, ih hsplit.2⟩
      | toolResult callId key =>
          rw [PromptAssembly.callsIn_cons_result row rest callId key hk]
          exact ih ((PromptAssembly.uniqueCallIds_cons_result row (rest ++ b) callId key hk).mp h)
      | ordinary =>
          rw [PromptAssembly.callsIn_cons_ordinary row rest hk]
          exact ih ((PromptAssembly.uniqueCallIds_cons_ordinary row (rest ++ b) hk).mp h)

/-- **The prefix-stability obligation.** The provider view of a longer history
begins with the provider view of the shorter one, provided the suffix
contributes no tool result for a call announced in the prefix.

The tail is existential on purpose: its identity is irrelevant to the
correspondence, which only needs `providerView a` to be a prefix. -/
theorem providerView_append (a b : List MessageRow)
    (hclean : Disjoint (resolvedIn (dropOrphanedFrom (pendingAfter ∅ (strip a)) (strip b)))
      (callsIn a)) :
    ∃ tail, providerView (a ++ b) = providerView a ++ tail := by
  have hDA : callsIn (dropOrphanedFrom ∅ (strip a)) = callsIn a := by
    rw [PromptAssembly.callsIn_dropOrphanedFrom, callsIn_strip]
  have hdisj : Disjoint (resolvedIn (dropOrphanedFrom (pendingAfter ∅ (strip a)) (strip b)))
      (callsIn (dropOrphanedFrom ∅ (strip a))) := by
    rw [hDA]; exact hclean
  refine ⟨filterCallsBy
      (resolvedIn (dropOrphanedFrom (pendingAfter ∅ (strip a)) (strip b)) ∪
        resolvedIn (dropOrphanedFrom ∅ (strip a)))
      (dropOrphanedFrom (pendingAfter ∅ (strip a)) (strip b)), ?_⟩
  unfold providerView PromptAssembly.sanitize PromptAssembly.dropUnpairedCalls
    PromptAssembly.dropOrphanedResults
  rw [strip_append, dropOrphanedFrom_append, resolvedIn_append, filterCallsBy_append,
    Finset.union_comm, PromptAssembly.filterCallsBy_irrelevant _ _ _ hdisj]

theorem resolvedIn_dropOrphaned_subset (l : List MessageRow) :
    resolvedIn (dropOrphanedFrom ∅ l) ⊆ callsIn l := by
  simpa using PromptAssembly.resolvedIn_dropOrphanedFrom_subset l ∅

/-- Sufficient condition 1: the prefix ends at a turn boundary. -/
theorem providerView_append_of_turn_boundary (a b : List MessageRow)
    (huniq : UniqueCallIds (a ++ b)) (hb : pendingAfter ∅ a = ∅) :
    ∃ tail, providerView (a ++ b) = providerView a ++ tail := by
  refine providerView_append a b ?_
  rw [pendingAfter_strip, hb]
  have hsub : resolvedIn (dropOrphanedFrom ∅ (strip b)) ⊆ callsIn b := by
    rw [← callsIn_strip b]
    exact resolvedIn_dropOrphaned_subset (strip b)
  exact ((uniqueCallIds_append_disjoint huniq).symm).mono_left hsub

/-- Sufficient condition 2, the one production satisfies: the suffix opens with
an ordinary row, because a new request appends its user prompt before anything
else. No result in the suffix can then attach to a call in the prefix. -/
theorem providerView_append_of_ordinary_start (a : List MessageRow) (row : MessageRow)
    (rest : List MessageRow) (huniq : UniqueCallIds (a ++ row :: rest))
    (hrow : row.kind = .ordinary) :
    ∃ tail, providerView (a ++ row :: rest) = providerView a ++ tail := by
  refine providerView_append a (row :: rest) ?_
  rw [strip_cons,
    PromptAssembly.dropOrphanedFrom_cons_ordinary (stripRow row) (strip rest) _
      (strip_kind_ordinary row hrow),
    PromptAssembly.resolvedIn_cons_ordinary (stripRow row) _ (strip_kind_ordinary row hrow)]
  have hsub : resolvedIn (dropOrphanedFrom ∅ (strip rest)) ⊆ callsIn (row :: rest) := by
    rw [PromptAssembly.callsIn_cons_ordinary row rest hrow, ← callsIn_strip rest]
    exact resolvedIn_dropOrphaned_subset (strip rest)
  exact ((uniqueCallIds_append_disjoint huniq).symm).mono_left hsub

theorem activeBlockValidFrom_append (a : List MessageRow) :
    ∀ (b : List MessageRow) (pending : Finset ToolExecution.ToolCallId),
      ActiveBlockValidFrom pending (a ++ b) →
        ActiveBlockValidFrom (pendingAfter pending a) b := by
  induction a with
  | nil => intro b pending h; exact h
  | cons row rest ih =>
      intro b pending h
      cases hk : row.kind with
      | toolResult callId key =>
          have hsplit :=
            (PromptAssembly.activeBlockValidFrom_cons_result row (rest ++ b) pending callId key
              hk).mp h
          rw [pendingAfter_cons_result row rest pending callId key hk, if_pos hsplit.1]
          exact ih b (pending.erase callId) hsplit.2
      | assistantToolCalls callIds =>
          have hsplit :=
            (PromptAssembly.activeBlockValidFrom_cons_assistant row (rest ++ b) pending callIds
              hk).mp h
          rw [pendingAfter_cons_assistant row rest pending callIds hk]
          exact ih b callIds hsplit.2
      | ordinary =>
          have hsplit :=
            (PromptAssembly.activeBlockValidFrom_cons_ordinary row (rest ++ b) pending hk).mp h
          rw [pendingAfter_cons_ordinary row rest pending hk]
          exact ih b ∅ hsplit.2

/-- Dropping at a pending-empty index of a provider view leaves a provider view.

This is what makes it safe for `agent/daemon/request.rs` to drop the compacted
prefix *after* sanitization without re-sanitizing: the writer's boundary is
always `pairSafeBoundary`, so the drop lands where nothing is pending. -/
theorem drop_preserves_providerValid (msgs : List MessageRow) (n : Nat)
    (hvalid : ProviderValid msgs) (hboundary : pendingAfter ∅ (msgs.take n) = ∅) :
    ProviderValid (msgs.drop n) := by
  constructor
  have hsplit : msgs.take n ++ msgs.drop n = msgs := List.take_append_drop n msgs
  have h := activeBlockValidFrom_append (msgs.take n) (msgs.drop n) ∅
    (by rw [hsplit]; exact hvalid.activeBlockValid)
  rwa [hboundary] at h

/-- **The correspondence the production fix rests on.** The count the compaction
writer records against `providerView H` names exactly the rows the next
request's reader drops from `providerView (H ++ new)`. -/
theorem compacted_prefix_correspondence
    {H new dropped old recent tail : List MessageRow}
    (hstable : providerView (H ++ new) = providerView H ++ tail)
    (hsplit : providerView H = dropped ++ old ++ recent) :
    (providerView (H ++ new)).drop (dropped.length + old.length) = recent ++ tail := by
  rw [hstable, hsplit]
  have hregroup : dropped ++ old ++ recent ++ tail = (dropped ++ old) ++ (recent ++ tail) := by
    simp [List.append_assoc]
  rw [hregroup]
  exact List.drop_left' (by simp)

end Compaction
