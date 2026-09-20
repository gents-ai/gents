import Proofs.CanonicalOutput.Execution.Compaction
import Proofs.CanonicalOutput.Execution.Examples
import Proofs.CanonicalOutput.Execution.ToolDeliveryCases

namespace CanonicalOutput.Execution.Compaction.Examples

open CanonicalOutput.Execution.Examples

def resultSegment : Segment :=
  { id := 700, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
    flush := some ⟨0, [⟨0, 1, some { block := 0, part := 0, kind := .toolOutput }⟩], [65]⟩
    close := some (.closed .complete 1 [1]), createdAt := 6 }

def resultMessage : MessageEnvelope :=
  { header :=
      { id := 701, session := 1, request := some 10, origin := none
        refs := [⟨700, 0⟩], outcome := .complete, role := .user
        publication := .toolDelivery 600 }
    key := "result-600", sequence := 1, nativeId := none, createdAt := 6
    blocks := [.toolResult 600 "native-call" none [.text ⟨⟨700, 0⟩, .full⟩]] }

def boundary : MessageEnvelope := { authoredMessage with sequence := 2 }

/-- An authoritative published snapshot; the execution traces separately prove
how these immutable artifacts commit. Physical document 600 is deliberately
unrelated to the native key "native-call". -/
def published : World :=
  { world 6 with
    segments := [providerTurn, resultSegment, authored]
    messages := [providerMessage, resultMessage, boundary]
    transcript := { transcript with nextSeq := 3 } }

def nativePairAndBoundaryCompactable : Bool :=
  match prefixView? published 2 with
  | none => false
  | some view =>
      decide (_root_.Compaction.PromptView.safeToReduce view) &&
        view.messages.all (_root_.Compaction.PromptView.rowPublished view) &&
        match view.messages with
        | [assistant, result, ordinary] =>
            assistant.kind == .assistantToolCalls
              {_root_.Compaction.PromptView.providerCallSymbol "native-call" none} &&
              result.kind == .toolResult
                (_root_.Compaction.PromptView.providerCallSymbol "native-call" none)
                ⟨1, 600, 701⟩ && ordinary.kind == .ordinary
        | _ => false

theorem physical_tool_ids_are_not_provider_symbols :
    nativePairAndBoundaryCompactable = true := by native_decide

theorem missing_payload_rejects_entire_prefix :
    (prefixView? { published with segments := [providerTurn, authored] } 2).isNone = true := by
  native_decide

theorem conflicting_header_rejects_entire_prefix :
    (prefixView? { published with messages := published.messages ++
      [{ resultMessage with key := "conflicting-result" }] } 2).isNone = true := by
  native_decide

def textNotification : MessageEnvelope :=
  { resultMessage with blocks := [.text ⟨⟨700, 0⟩, .full⟩] }

theorem notification_projects_as_ordinary_not_physical_result :
    (publishedRow? { published with
      messages := [providerMessage, textNotification, boundary] } textNotification).map
        (·.kind) = some .ordinary := by native_decide

def copied (message : MessageEnvelope) : MessageEnvelope :=
  { message with
    header := forkHeader message.header (message.header.id + 1000) 2
    key := "fork-" ++ message.key }

def forked : World :=
  { published with
    sessionId := 2
    messages := published.messages ++ published.messages.map copied
    transcript := { transcript with sessionId := 2, nextSeq := 3 } }

theorem forked_native_results_remain_compactable :
    (match prefixView? forked 2 with
      | none => false
      | some view => decide (_root_.Compaction.PromptView.safeToReduce view)) = true := by
  native_decide

theorem fork_without_origin_cannot_be_compacted :
    (prefixView? { forked with messages := published.messages.map copied } 2).isNone = true := by
  native_decide

theorem cannot_advance_cursor_past_allocator :
    advanceCursor? published 3 = none := by native_decide

theorem cannot_compact_unpaired_or_missing_output :
    advanceCursor? { published with segments := [providerTurn, authored] } 2 = none := by
  native_decide

theorem cannot_rewind_committed_cursor :
    advanceCursor? { published with compactionCursor := some 2 } 1 = none := by
  native_decide

/-- The same owner admits, dispatches, closes, replies, publishes an ordinary
boundary, and advances the cursor. No seeded delivery row or second allocator
is used to make the compaction preconditions true. -/
def executionThroughCompaction : Option Bool := do
  let accepted ← (acceptAndPublish (world 5) 7 providerTurn providerMessage []
    [foregroundAdmission]).toOption
  let dispatched ← (dispatch accepted 7 permit).toOption
  let closed ← (ToolDelivery.closeToolOutput dispatched 600 (.native .complete)
    ToolDelivery.Cases.toolOutputClose).toOption
  let delivered ← (ToolDelivery.publishToolDelivery closed 600
    (ToolDelivery.Cases.foregroundResultMessage 1)).toOption
  let bounded ← (publishAuthored delivered 7 authored boundary).toOption
  let compacted ← advanceCursor? bounded 2
  let replay ← advanceCursor? compacted 2
  pure (compacted.compactionCursor == some 2 &&
    compacted.transcript.nextSeq == 3 && replay == compacted)

theorem execution_delivery_and_compaction_share_one_sequence :
    executionThroughCompaction = some true := by native_decide

def receiptSnapshot : World :=
  { published with
    segments := [providerTurn, ToolDelivery.Cases.backgroundReceiptClose, authored]
    messages := [providerMessage, ToolDelivery.Cases.backgroundReceiptMessage, boundary] }

def forkedReceiptSnapshot : World :=
  { receiptSnapshot with
    sessionId := 2
    messages := receiptSnapshot.messages ++ receiptSnapshot.messages.map copied
    transcript := { transcript with sessionId := 2, nextSeq := 3 } }

theorem forked_background_receipt_preserves_native_pairing :
    (advanceCursor? forkedReceiptSnapshot 2).isSome = true := by native_decide

end CanonicalOutput.Execution.Compaction.Examples
