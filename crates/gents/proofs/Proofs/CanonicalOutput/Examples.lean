import Proofs.CanonicalOutput.Terminal
import Proofs.RequestExecutionLease.Transition

/-! Executable witnesses for the new model, not regenerated Rust fixtures. -/
namespace CanonicalOutput.Examples

local instance {ε α : Type} [DecidableEq ε] [DecidableEq α] :
    DecidableEq (Except ε α)
  | .error a, .error b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.error.inj e))
  | .error _, .ok _ => .isFalse nofun
  | .ok _, .error _ => .isFalse nofun
  | .ok a, .ok b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.ok.inj e))

def coordinate : Coordinate := ⟨10, .provider 0 0 0⟩
def textDeclaration : Declaration := { block := 0, part := 0, kind := .text }
def argumentDeclaration : Declaration :=
  { block := 1, part := 0, kind := .arguments, tool := some ⟨"call", none, "child"⟩ }
def laterTextDeclaration : Declaration := { block := 2, part := 0, kind := .text }

/-- Text followed by deliberately incomplete argument JSON (`{`). -/
def finalFlush : Segment :=
  { id := 100
    coordinate := coordinate
    writer := .request 7
    flush := some ⟨0, [⟨0, 1, some textDeclaration⟩,
                       ⟨1, 1, some argumentDeclaration⟩], [65, 123]⟩
    close := some (.closed .«partial» 1 [1, 1])
    createdAt := 5 }

theorem incomplete_arguments_do_not_erase_text :
    reconstructExtent [finalFlush] finalFlush =
      .ok [(textDeclaration, [65]), (argumentDeclaration, [123])] := by native_decide

theorem recovery_omits_arguments_without_rewriting_bytes :
    recoveryText [(0, textDeclaration), (1, argumentDeclaration)] =
      .ok [(0, textDeclaration)] := rfl

theorem recovery_sorts_native_positions_and_preserves_stream_ids :
    recoveryText [(7, laterTextDeclaration), (3, textDeclaration)] =
      .ok [(3, textDeclaration), (7, laterTextDeclaration)] := rfl

def outOfOrderOpeningFlush : Segment :=
  { finalFlush with
    flush := some ⟨0, [⟨0, 1, some argumentDeclaration⟩,
                       ⟨1, 1, some textDeclaration⟩], [123, 65]⟩ }

theorem stream_openings_may_arrive_out_of_native_order :
    reconstructExtent [outOfOrderOpeningFlush] outOfOrderOpeningFlush =
      .ok [(argumentDeclaration, [123]), (textDeclaration, [65])] := by native_decide

theorem missing_prefix_is_not_short_output :
    reconstructExtent [] finalFlush = .error (.missingOrdinal 0) := rfl

theorem different_document_same_ordinal_conflicts :
    reconstructExtent [finalFlush, { finalFlush with id := 101 }] finalFlush =
      .error (.conflictingOrdinal 0) := rfl

theorem closure_twin_is_not_hidden_by_exact_reference :
    resolveClose [finalFlush, { finalFlush with id := 101 }] [] ⟨100, 0⟩ =
      .error .conflictingClosures := rfl

theorem exact_duplicate_segment_delivery_is_idempotent :
    resolveClose [finalFlush, finalFlush] [] ⟨100, 0⟩ = .ok finalFlush ∧
      reconstructExtent [finalFlush, finalFlush] finalFlush =
        .ok [(textDeclaration, [65]), (argumentDeclaration, [123])] := by native_decide

theorem owner_verified_absent_dependency_denial_is_not_loading :
    reconstructPayload [] [] ⟨100, 0⟩ [⟨100, 101⟩] =
      .error (.lookup .denied) := rfl

def late : Segment :=
  { finalFlush with id := 102, flush := some ⟨1, [⟨0, 1, none⟩], [66]⟩, close := none }

theorem late_raw_bytes_do_not_extend_recovery :
    reconstructExtent [finalFlush, late] finalFlush =
      reconstructExtent [finalFlush] finalFlush := rfl

def providerWithToolWriter : Segment :=
  { finalFlush with writer := .tool 42 }

theorem provider_source_rejects_tool_writer :
    reconstructExtent [providerWithToolWriter] providerWithToolWriter =
      .error .invalidWriter := rfl

def toolFlush : Segment :=
  { finalFlush with coordinate := ⟨10, .tool 42⟩, writer := .tool 42 }

theorem tool_source_accepts_its_exact_writer :
    reconstructExtent [toolFlush] toolFlush =
      .ok [(textDeclaration, [65]), (argumentDeclaration, [123])] := by native_decide

theorem tool_source_rejects_a_different_tool_writer :
    let wrong := { toolFlush with writer := .tool 43 }
    reconstructExtent [wrong] wrong = .error .invalidWriter := rfl

def delegatedFlush : Segment :=
  { finalFlush with
    flush := some ⟨0, [⟨0, 1, some textDeclaration⟩,
                       ⟨1, 2, some argumentDeclaration⟩], [65, 123, 125]⟩
    close := some (.closed .complete 1 [1, 2]) }

/-- Document ACP cannot give a delegate stream 1's bytes while hiding stream 0
inside the same document. A PayloadRef is not a smaller authorization unit. -/
theorem argument_document_also_contains_other_content :
    reconstructPayload [delegatedFlush] [] ⟨100, 1⟩ =
      .ok (argumentDeclaration, [123, 125]) ∧
      delegatedFlush.flush.map (·.payload) = some [65, 123, 125] := by native_decide

theorem utf8_cannot_be_split_between_runs :
    consumeRuns [⟨0, 1, some textDeclaration⟩, ⟨0, 1, none⟩] [195, 169] [] =
      .error .malformedRuns := by native_decide

theorem complete_utf8_run_is_preserved :
    consumeRuns [⟨0, 2, some textDeclaration⟩] [195, 169] [] =
      .ok [(textDeclaration, [195, 169])] := by native_decide

theorem empty_continuation_is_not_progress :
    consumeRuns [⟨0, 0, none⟩] [] [(textDeclaration, [65])] =
      .error .malformedRuns := by native_decide

def assistantHeader : Header :=
  { id := 200
  , session := 1
  , request := some 10
  , origin := none
  , refs := [⟨100, 0⟩]
  , outcome := .complete
  , role := .assistant
  , publication := .requestExecution 7
  }

theorem eligible_assistant_is_selected_exactly :
    resolveTerminal [assistantHeader] [] 10 1 (some (.message 200)) =
      .ok (some assistantHeader) := rfl

theorem exact_duplicate_header_delivery_is_idempotent :
    resolveTerminal [assistantHeader, assistantHeader] [] 10 1 (some (.message 200)) =
      .ok (some assistantHeader) := rfl

theorem distinct_header_at_selected_identity_conflicts :
    let conflicting := { assistantHeader with role := .user }
    resolveTerminal [assistantHeader, conflicting] [] 10 1 (some (.message 200)) =
      .error .conflictingHeaders := rfl

theorem user_header_cannot_be_terminal_output :
    let user := { assistantHeader with role := .user }
    resolveTerminal [user] [] 10 1 (some (.message 200)) =
      .error .ineligibleHeader := rfl

theorem tool_delivery_header_cannot_be_terminal_output :
    let delivery := { assistantHeader with publication := .toolDelivery 300 }
    resolveTerminal [delivery] [] 10 1 (some (.message 200)) =
      .error .ineligibleHeader := rfl

open RequestExecutionLease

def live : World Nat :=
  { (initial Nat) with
    request := .processing
    lease := .active 7 10 10
    usedGenerations := [7]
    now := 5 }

theorem output_append_is_admission_not_renewal :
    step? live (.appendOutput .mutationWriteGate 7) = some live ∧
      effectiveExpiry live = 10 := by native_decide

def renewed : World Nat := { live with lease := .active 7 10 15 }

theorem due_silent_owner_renews_with_exact_deadline_cas :
    step? live (.renew .mutationWriteGate 7 10) = some renewed := by native_decide

theorem early_renewal_is_rejected :
    step? { live with now := 4 } (.renew .mutationWriteGate 7 10) = none := by
  native_decide

theorem acknowledged_renewal_replay_with_stale_deadline_is_rejected :
    step? renewed (.renew .mutationWriteGate 7 10) = none := by native_decide

def renewedAgain : World Nat := { renewed with now := 10, lease := .active 7 10 20 }

theorem finite_silent_owner_trace_can_renew_repeatedly :
    step? live (.renew .mutationWriteGate 7 10) = some renewed ∧
      step? { renewed with now := 10 } (.renew .mutationWriteGate 7 15) =
        some renewedAgain := by native_decide

theorem exact_deadline_is_expired_without_generation_swap :
    step? { live with now := 10 } (.appendOutput .mutationWriteGate 7) = none ∧
      step? { live with now := 10 } (.renew .mutationWriteGate 7 10) = none ∧
      step? { live with now := 10 }
        (.authorizeProducerDecision .mutationWriteGate 7 .dispatch) = none := by
  native_decide

theorem stale_generation_cannot_explicitly_renew :
    step? live (.renew .mutationWriteGate 6 10) = none := by native_decide

theorem producer_decision_does_not_rewrite_deadline :
    step? live (.authorizeProducerDecision .mutationWriteGate 7 .dispatch) = some live := by
  native_decide

end CanonicalOutput.Examples
