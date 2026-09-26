import Proofs.CanonicalOutput.Execution.GateCases
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime

/-!
# Payload presentation before provider projection (#1571)

Headers select presentations of immutable streams. This fixture compares stored
payload length with reconstructed payload length, not serialized provider input
length or token usage. Neither payload quantity bounds the other.

The existing `provider_input` owner projects complete native messages into a
provider-specific request and estimates that serialized body, including metadata,
escaping, tool schemas and other request fields. These fixtures must not replace
that owner with a sum of payload sizes. The threshold/projection join has its
own model and fixtures; no tokenizer or reduction decision is modeled here.
-/
namespace Conformance.PayloadPresentationContracts

open CanonicalOutput
open CanonicalOutput.Execution.Examples
open CanonicalOutput.Execution.Gate.Cases
open Conformance.Contracts

structure PayloadLengths where
  storedStreamBytes : Nat
  presentedBytes : Nat
  deriving DecidableEq, Repr

def mediaPayloads (media : Media (List UInt8)) : List (List UInt8) :=
  match media.data with
  | .base64 payload | .raw payload | .string payload => [payload]
  | _ => []

def blockPayloads : MessageBlock (List UInt8) → List (List UInt8)
  | .text payload => [payload]
  | .reasoning _ parts => parts.map fun
      | .text payload _ | .encrypted payload | .redacted payload | .summary payload => payload
  | .toolCall _ _ _ _ arguments _ _ => [arguments]
  | .toolResult _ _ _ parts => parts.flatMap fun
      | .text payload => [payload]
      | .media value => mediaPayloads value
  | .media value => mediaPayloads value

/-- Lengths of reconstructed payload fields only. Inline identifiers, media URLs,
signatures and provider serialization overhead are outside this measurement.
Missing dependencies produce no measurement, never a shorter fallback. -/
def payloadLengths (records : List Segment) (message : MessageEnvelope) : Option PayloadLengths := do
  let native ← (reconstructMessage records [] message).toOption
  let stored ← (envelopeRefs message).dedup.mapM fun reference =>
    (reconstructPayload records [] reference).toOption.map (·.2.length)
  pure ⟨stored.sum, ((native.blocks.flatMap blockPayloads).map (·.length)).sum⟩

def windowedOutput : Segment :=
  { id := 800, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
    flush := some ⟨0,
      [⟨0, 10, some { block := 0, part := 0, kind := .toolOutput }⟩],
      [48, 49, 50, 51, 52, 53, 54, 55, 56, 57]⟩
    close := some (.closed .complete 1 [10]), createdAt := 5 }

/-- Head two bytes, a three-byte UTF-8 ellipsis marker, tail two bytes. -/
def windowedResult : MessageEnvelope :=
  { header :=
      { id := 801, session := 1, request := some 10, origin := none
        refs := [⟨800, 0⟩], outcome := .complete, role := .user
        publication := .toolDelivery 600 }
    key := "windowed-result-600", sequence := 1, nativeId := none, createdAt := 5
    blocks := [.toolResult 600 "native-call" none
      [.text ⟨⟨800, 0⟩, .composed [.range 0 2, .literal [226, 128, 166], .range 8 10]⟩]] }

structure Case where
  name : String
  segments : List Segment
  message : MessageEnvelope
  expected : Option PayloadLengths

def mkCase (name : String) (segments : List Segment) (message : MessageEnvelope) : Case :=
  ⟨name, segments, message, payloadLengths segments message⟩

def cases : List Case :=
  [ mkCase "full_presentation_retains_payload_length" [providerTurn] providerMessage
  , mkCase "inline_literals_present_more_than_stored" [toolOutputClose]
      (foregroundResultMessage 1)
  , mkCase "head_tail_window_presents_less_than_stored" [windowedOutput] windowedResult
  , mkCase "missing_dependency_has_no_payload_measurement" [] windowedResult ]

def payloadLengthsJson : Option PayloadLengths → String
  | none => "null"
  | some value =>
      "{\"stored_payload_bytes\":" ++ toString value.storedStreamBytes ++
        ",\"presented_payload_bytes\":" ++ toString value.presentedBytes ++ "}"

def caseJson (value : Case) : String :=
  "{" ++ "\"name\":" ++ jsonString value.name ++ ","
    ++ "\"segments\":" ++ jsonArray (value.segments.map canonicalSegmentJson) ++ ","
    ++ "\"message\":" ++ canonicalMessageJson value.message ++ ","
    ++ "\"expected\":" ++ payloadLengthsJson value.expected ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

/-- The exported boundary really is two-sided, and it fails closed. -/
example : (cases.map (·.expected)) =
    [some ⟨3, 3⟩, some ⟨1, 3⟩, some ⟨10, 7⟩, none] := by native_decide

example : cases.any (fun value => value.expected.any fun s => s.presentedBytes < s.storedStreamBytes)
    && cases.any (fun value => value.expected.any fun s => s.storedStreamBytes < s.presentedBytes)
    = true := by native_decide

end Conformance.PayloadPresentationContracts

namespace Conformance.TerminalDiagnosticContracts

open CanonicalOutput Conformance.Contracts

structure Case where
  name : String
  raw : List UInt8
  cause : List UInt8
  tailBudget : Nat

def result (value : Case) : Except MessageError (Presentation × List UInt8) := do
  let presentation ← terminalDiagnosticPresentation value.raw value.cause value.tailBudget
  return (presentation, ← present value.raw presentation)

def cases : List Case :=
  [ ⟨"empty_output_retains_cause", [], [116], 16⟩
  , ⟨"whole_prefix_then_cause", [65, 66], [116], 16⟩
  , ⟨"bounded_ascii_tail", [65, 66, 67, 68], [116], 2⟩
  , ⟨"zero_budget_retains_only_cause", [65, 66], [116], 0⟩
  , ⟨"utf8_boundary_moves_forward", [65, 226, 130, 172, 66], [116], 3⟩
  , ⟨"utf8_exact_budget_retains_codepoint", [65, 226, 130, 172, 66], [116], 4⟩
  , ⟨"budget_smaller_than_final_codepoint", [65, 226, 130, 172], [116], 2⟩
  , ⟨"invalid_raw_rejected", [255], [116], 16⟩
  , ⟨"invalid_cause_rejected", [65], [255], 16⟩
  , ⟨"multibyte_cause_preserved", [65], [226, 130, 172], 1⟩ ]

def resultJson : Except MessageError (Presentation × List UInt8) → String
  | .ok (presentation, rendered) =>
      "{\"kind\":\"ok\",\"presentation\":" ++ presentationJson presentation ++
      ",\"rendered\":" ++ byteArrayJson rendered ++ "}"
  | .error .invalidUtf8 => "{\"kind\":\"invalid_utf8\"}"
  | .error error =>
      "{\"kind\":\"unexpected_error\",\"error\":" ++ jsonString (reprStr error) ++ "}"

def caseJson (value : Case) : String :=
  "{\"name\":" ++ jsonString value.name ++
  ",\"raw\":" ++ byteArrayJson value.raw ++
  ",\"cause\":" ++ byteArrayJson value.cause ++
  ",\"tail_budget\":" ++ toString value.tailBudget ++
  ",\"expected\":" ++ resultJson (result value) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

example : (cases.map fun value => (result value).toOption.map Prod.snd) =
    [some [116], some [65, 66, 10, 116], some [67, 68, 10, 116], some [116],
     some [66, 10, 116], some [226, 130, 172, 66, 10, 116], some [116],
     none, none, some [65, 10, 226, 130, 172]] := by native_decide

end Conformance.TerminalDiagnosticContracts

namespace Conformance.TerminalDiagnosticReplayContracts

open CanonicalOutput Conformance.Contracts

/-- A delivered interrupted-call diagnostic is replayed exactly: the stored
presentation must be `terminalDiagnosticPresentation raw cause b` for some tail
budget `b`. The budget in force at replay (`configuredBudget`) does not decide
acceptance, because configuration may change between delivery and replay; any
other stored shape (inlined literals, `full`, a non-suffix range) is rejected. -/
structure Case where
  name : String
  raw : List UInt8
  cause : List UInt8
  configuredBudget : Nat
  stored : Presentation

def accepted (value : Case) : Bool :=
  (List.range (value.raw.length + 1)).any fun budget =>
    match terminalDiagnosticPresentation value.raw value.cause budget with
    | .ok presentation => presentation == value.stored
    | .error _ => false

private def raw20 : List UInt8 := "0123456789abcdefghij".toUTF8.toList
private def cause : List UInt8 := "tool call deadline exceeded".toUTF8.toList
private def modeled (raw : List UInt8) (budget : Nat) : Presentation :=
  match terminalDiagnosticPresentation raw cause budget with
  | .ok presentation => presentation
  | .error _ => .full

def cases : List Case :=
  [ ⟨"delivered_tail_5_replayed_under_12", raw20, cause, 12, modeled raw20 5⟩
  , ⟨"delivered_tail_12_replayed_under_5", raw20, cause, 5, modeled raw20 12⟩
  , ⟨"delivered_cause_only_replayed_under_16", raw20, cause, 16, modeled raw20 0⟩
  , ⟨"delivered_whole_source_replayed_under_1", raw20, cause, 1, modeled raw20 64⟩
  , ⟨"utf8_tail_replayed_under_default", "A€B".toUTF8.toList, cause, 16000,
      modeled "A€B".toUTF8.toList 4⟩
  , ⟨"literal_inlined_tail_rejected", raw20, cause, 5,
      .composed [.literal ("fghij\n".toUTF8.toList ++ cause)]⟩
  , ⟨"all_literal_parts_rejected", raw20, cause, 5,
      .composed [.literal "fghij".toUTF8.toList, .literal [10], .literal cause]⟩
  , ⟨"full_presentation_rejected", raw20, cause, 5, .full⟩
  , ⟨"non_suffix_range_with_suffix_bytes_rejected", "abab".toUTF8.toList, cause, 2,
      .composed [.range 0 2, .literal [10], .literal cause]⟩
  , ⟨"range_short_of_source_end_rejected", raw20, cause, 5,
      .composed [.range 15 19, .literal [10], .literal cause]⟩
  , ⟨"range_off_character_boundary_rejected", "A€B".toUTF8.toList, cause, 3,
      .composed [.range 2 5, .literal [10], .literal cause]⟩
  , ⟨"different_cause_rejected", raw20, cause, 5,
      .composed [.range 15 20, .literal [10], .literal "tool call cancelled".toUTF8.toList]⟩ ]

def caseJson (value : Case) : String :=
  "{\"name\":" ++ jsonString value.name ++
  ",\"raw\":" ++ byteArrayJson value.raw ++
  ",\"cause\":" ++ byteArrayJson value.cause ++
  ",\"configured_budget\":" ++ toString value.configuredBudget ++
  ",\"stored\":" ++ presentationJson value.stored ++
  ",\"accepted\":" ++ (if accepted value then "true" else "false") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

example : cases.map accepted =
    [true, true, true, true, true, false, false, false, false, false, false, false] := by
  native_decide

end Conformance.TerminalDiagnosticReplayContracts
