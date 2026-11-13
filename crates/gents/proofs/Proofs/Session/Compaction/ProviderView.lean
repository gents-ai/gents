import Proofs.Session.Compaction.Strip
import Proofs.Session.PromptAssembly.Properties

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

theorem strip_sanitize_commute (msgs : List MessageRow) :
    strip (sanitizeGlobal msgs) = sanitizeGlobal (strip msgs) := by
  have hres : resolvedIn (dropOrphanedFrom ∅ (strip msgs))
      = resolvedIn (dropOrphanedFrom ∅ msgs) := by
    rw [← strip_dropOrphanedFrom, resolvedIn_strip]
  unfold PromptAssembly.sanitizeGlobal PromptAssembly.dropUnpairedCalls
    PromptAssembly.dropOrphanedResults
  rw [strip_filterCallsBy, strip_dropOrphanedFrom, hres]

def providerViewGlobal (msgs : List MessageRow) : List MessageRow := sanitizeGlobal (strip msgs)

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

theorem providerViewGlobal_idempotent {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    providerViewGlobal (providerViewGlobal msgs) = providerViewGlobal msgs := by
  unfold providerViewGlobal
  rw [strip_sanitize_commute, strip_idempotent]
  exact PromptAssembly.sanitizeGlobal_idempotent (strip_preserves_uniqueCallIds huniq)

theorem providerViewGlobal_nonempty_announcements (msgs : List MessageRow) :
    PromptAssembly.NonemptyAnnouncements (providerViewGlobal msgs) :=
  PromptAssembly.nonemptyAnnouncements_sanitize _

theorem strip_providerView (msgs : List MessageRow) :
    strip (providerViewGlobal msgs) = providerViewGlobal msgs := by
  unfold providerViewGlobal
  rw [strip_sanitize_commute, strip_idempotent]

/-! ## Call keys scoped to their turn

A result closes an announcement only inside its own turn, so a key reused by a
later turn is resolved there and nowhere else. -/

inductive ProviderViewStep where
  | calls (callIds : List ToolExecution.ToolCallId)
  | result (callId : ToolExecution.ToolCallId)
  | ordinary

def callScopeRowsFrom (index : Nat) : List ProviderViewStep → List MessageRow
  | [] => []
  | .calls callIds :: rest =>
      ⟨index, 0, index, .assistant, .assistantToolCalls callIds.toFinset⟩
        :: callScopeRowsFrom (index + 1) rest
  | .result callId :: rest =>
      ⟨index, 0, index, .user, .toolResult callId ⟨0, 0, 0⟩⟩
        :: callScopeRowsFrom (index + 1) rest
  | .ordinary :: rest =>
      ⟨index, 0, index, .user, .ordinary⟩ :: callScopeRowsFrom (index + 1) rest

def callScopeRows (steps : List ProviderViewStep) : List MessageRow :=
  callScopeRowsFrom 0 steps

def callCountOf : List MessageRow → Nat
  | [] => 0
  | row :: rest =>
      (match row.kind with
        | .assistantToolCalls callIds => callIds.card
        | _ => 0) + callCountOf rest

def resultCountOf : List MessageRow → Nat
  | [] => 0
  | row :: rest =>
      (match row.kind with
        | .toolResult _ _ => 1
        | _ => 0) + resultCountOf rest

def callKeyReuseAcrossTurns : List ProviderViewStep :=
  [.calls [1], .result 1, .calls [1], .result 1]

def incompleteSecondTurnReusingAKey : List ProviderViewStep :=
  [.calls [1], .result 1, .calls [1]]

theorem callKeyReuseAcrossTurnsSurvives :
    sanitizeTurn (callScopeRows callKeyReuseAcrossTurns)
      = callScopeRows callKeyReuseAcrossTurns := by
  native_decide

theorem incompleteSecondTurnReusingAKeyIsNotResolvedByTheFirst :
    (callCountOf (sanitizeTurn (callScopeRows incompleteSecondTurnReusingAKey)),
      resultCountOf (sanitizeTurn (callScopeRows incompleteSecondTurnReusingAKey)))
      = (1, 1) := by
  native_decide

end Compaction
