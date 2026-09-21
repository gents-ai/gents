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
