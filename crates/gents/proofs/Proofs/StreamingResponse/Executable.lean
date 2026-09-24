import Proofs.StreamingResponse.Properties
import Proofs.StreamingResponse.TargetSelection

namespace StreamingResponse

open CanonicalOutput

structure TargetSelectionCase where
  name : String
  request : Nat
  generation : Nat
  inferenceScopes : List Nat
  records : List Segment
  messages : List MessageEnvelope
  expected : TargetSelection
  deriving Repr

def sampleCoordinate : Coordinate := ⟨10, .provider 0 0 0⟩
def sampleText : Declaration := { block := 0, part := 0, kind := .text }
def sampleOpaque : Declaration := { block := 1, part := 0, kind := .encrypted }

def sampleComplete : Segment :=
  { id := 100, coordinate := sampleCoordinate, writer := .request 7
    flush := some ⟨0,
      [⟨0, 1, some sampleText⟩, ⟨1, 1, some sampleOpaque⟩], [65, 66]⟩
    close := some (.closed .complete 1 [1, 1]), createdAt := 5 }

def samplePartial : Segment :=
  { sampleComplete with id := 101, close := some (.closed .partial 1 [1, 1]) }

def sampleOpen : Segment := { sampleComplete with id := 102, close := none }
def sampleRetracted : Segment :=
  { sampleComplete with id := 103, flush := none, close := some .retracted }

/-- A benign late raw flush is outside the already committed one-segment
extent, carries no competing closure, and has a distinct immutable identity. -/
def sampleLateBeyondExtent : Segment :=
  { sampleComplete with
    id := 104
    flush := some ⟨1, [⟨0, 1, none⟩], [90]⟩
    close := none
    createdAt := 6 }

def sampleLateInExtentTwin : Segment :=
  { sampleComplete with
    id := 105
    flush := some ⟨0, [⟨0, 1, some sampleText⟩], [90]⟩
    close := none
    createdAt := 6 }

def sampleOpenContinuation : Segment :=
  { sampleOpen with
    id := 106
    flush := some ⟨1, [⟨0, 1, none⟩, ⟨1, 1, none⟩], [67, 68]⟩
    createdAt := 6 }

/-- The committed extent exists, but its declared run lengths consume more
bytes than the immutable payload contains. -/
def sampleMalformedSealed : Segment :=
  { sampleComplete with
    flush := some ⟨0,
      [⟨0, 2, some sampleText⟩, ⟨1, 1, some sampleOpaque⟩], [65, 66]⟩ }

def textSpec : PayloadSpec := ⟨⟨100, 0⟩, .full⟩
def opaqueSpec : PayloadSpec := ⟨⟨100, 1⟩, .full⟩

def sampleMessage : MessageEnvelope :=
  { header :=
      { id := 200, session := 1, request := some 10, origin := none
        refs := [textSpec.reference, opaqueSpec.reference]
        outcome := .complete, role := .assistant, publication := .requestExecution 7 }
    key := "turn", sequence := 0, nativeId := some "native"
    blocks := [.text textSpec, .reasoning none [.encrypted opaqueSpec]]
    createdAt := 6 }

def sampleForkMessage : MessageEnvelope :=
  { sampleMessage with
    header := forkHeader sampleMessage.header 201 2
    key := "child-turn", sequence := 0 }

def sampleReasoning : Declaration := { block := 0, part := 0, kind := .reasoning }
def sampleReasoningSignature : Declaration := { block := 0, part := 0, kind := .signature }
def sampleArguments : Declaration :=
  { block := 1, part := 0, kind := .arguments
    tool := some ⟨"provider-call", some "call-alias", "lookup"⟩ }

/-- A native assistant payload which exercises both nested reasoning parts and
tool-call argument JSON. Expectations for this witness still come exclusively
from `project`/`reconstructMessage`; this is not a second projection policy. -/
def sampleNativePayload : Segment :=
  { id := 110, coordinate := sampleCoordinate, writer := .request 7
    flush := some ⟨0,
      [⟨0, 3, some sampleReasoning⟩, ⟨1, 2, some sampleArguments⟩,
       ⟨2, 3, some sampleReasoningSignature⟩],
      [119, 104, 121, 123, 125, 115, 105, 103]⟩
    close := some (.closed .complete 1 [3, 2, 3]), createdAt := 5 }

def sampleNativeMessage : MessageEnvelope :=
  { header :=
      { id := 210, session := 1, request := some 10, origin := none
        refs := [⟨110, 0⟩, ⟨110, 1⟩]
        outcome := .complete, role := .assistant, publication := .requestExecution 7 }
    key := "native-turn", sequence := 1, nativeId := some "assistant-native"
    blocks :=
      [.reasoning (some "reasoning-id") [.text ⟨⟨110, 0⟩, .full⟩ (some "sig")],
       .toolCall 42 "provider-call" (some "call-alias") "lookup"
          ⟨⟨110, 1⟩, .full⟩ (some "tool-sig") (some "{\"mode\":\"fast\"}")]
    createdAt := 7 }

def sampleMixedReasoningPayload : Segment :=
  { id := 120, coordinate := sampleCoordinate, writer := .request 7
    flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .reasoning }⟩,
       ⟨1, 1, some { block := 0, part := 1, kind := .summary }⟩,
       ⟨2, 1, some { block := 0, part := 2, kind := .encrypted }⟩,
       ⟨3, 1, some { block := 0, part := 3, kind := .redacted }⟩,
       ⟨4, 3, some sampleReasoningSignature⟩],
      [97, 98, 99, 100, 115, 105, 103]⟩
    close := some (.closed .complete 1 [1, 1, 1, 1, 3]), createdAt := 5 }

def sampleMixedReasoningMessage : MessageEnvelope :=
  { header :=
      { id := 220, session := 1, request := some 10, origin := none
        refs := [⟨120, 0⟩, ⟨120, 1⟩, ⟨120, 2⟩, ⟨120, 3⟩]
        outcome := .complete, role := .assistant, publication := .requestExecution 7 }
    key := "mixed-reasoning", sequence := 2, nativeId := some "mixed-native"
    blocks := [.reasoning (some "mixed-reasoning-id")
      [.text ⟨⟨120, 0⟩, .full⟩ (some "sig"),
       .summary ⟨⟨120, 1⟩, .full⟩,
       .encrypted ⟨⟨120, 2⟩, .full⟩,
       .redacted ⟨⟨120, 3⟩, .full⟩]]
    createdAt := 7 }

def sampleAllOpaqueReasoningPayload : Segment :=
  { id := 121, coordinate := sampleCoordinate, writer := .request 7
    flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .encrypted }⟩,
       ⟨1, 1, some { block := 0, part := 1, kind := .redacted }⟩],
      [101, 102]⟩
    close := some (.closed .complete 1 [1, 1]), createdAt := 5 }

def sampleAllOpaqueReasoningMessage : MessageEnvelope :=
  { header :=
      { id := 221, session := 1, request := some 10, origin := none
        refs := [⟨121, 0⟩, ⟨121, 1⟩]
        outcome := .complete, role := .assistant, publication := .requestExecution 7 }
    key := "all-opaque-reasoning", sequence := 3, nativeId := some "opaque-native"
    blocks := [.reasoning (some "opaque-reasoning-id")
      [.encrypted ⟨⟨121, 0⟩, .full⟩,
       .redacted ⟨⟨121, 1⟩, .full⟩]]
    createdAt := 7 }

def baseObservation
    (records : List Segment := [sampleComplete])
    (messages : List MessageEnvelope := [sampleMessage])
    (messageId : Option DocId := some 200)
    (owner : OwnerLiveness := ⟨some (10, 7), []⟩)
    (coordinate : Coordinate := sampleCoordinate)
    (writer : Writer := .request 7)
    (requestTerminal : Bool := false)
    (terminalSelection : Option TerminalSelection := none) : Observation :=
  { request := 10, session := 1, records, messages
  , deniedHeaders := [], deniedSegments := [], dependencyDenials := []
  , owner, target := ⟨coordinate, writer, messageId⟩
  , requestTerminal, terminalSelection }

def mixedReasoningObservation : Observation :=
  baseObservation (records := [sampleMixedReasoningPayload])
    (messages := [sampleMixedReasoningMessage]) (messageId := some 220)

def allOpaqueReasoningObservation : Observation :=
  baseObservation (records := [sampleAllOpaqueReasoningPayload])
    (messages := [sampleAllOpaqueReasoningMessage]) (messageId := some 221)

def viewName : View → String
  | .absent => "absent"
  | .live _ => "live"
  | .loading => "loading"
  | .settling _ => "settling"
  | .denied => "denied"
  | .conflicted => "conflicted"
  | .invalid => "invalid"
  | .retracted => "retracted"
  | .retainedPartial _ => "retained_partial"
  | .published _ _ => "published"

/-- The presentation projection intentionally excludes encrypted/redacted opaque
reasoning even though canonical reconstruction retains it losslessly. -/
def renderedKinds : View → List String
  | .live streams | .retainedPartial streams =>
      streams.filterMap fun stream => match stream.1.kind with
        | .signature | .encrypted | .redacted => none
        | .text => some "text"
        | .reasoning => some "reasoning"
        | .summary => some "summary"
        | .arguments => some "arguments"
        | .toolOutput => some "tool_output"
        | .media => some "media"
  | .published _ native =>
      native.blocks.flatMap fun block => match block with
        | .text _ => ["text"]
        | .reasoning _ parts => parts.filterMap fun part => match part with
            | .text _ _ => some "reasoning"
            | .summary _ => some "summary"
            | .encrypted _ | .redacted _ => none
        | .toolCall .. => ["arguments"]
        | .toolResult .. => ["tool_output"]
        | .media _ => ["media"]
  | _ => []

structure OutputProjectionCase where
  name : String
  input : Observation
  expected : View
  deriving Repr

private def case (name : String) (input : Observation) : OutputProjectionCase :=
  { name, input, expected := project input }

def outputProjectionCases : List OutputProjectionCase :=
  [ case "published_typed_message_filters_opaque_presentation" baseObservation
  , case "published_native_reasoning_and_tool_arguments"
      (baseObservation (records := [sampleNativePayload])
        (messages := [sampleNativeMessage]) (messageId := some 210))
  , case "malformed_sealed_payload_is_invalid"
      (baseObservation (records := [sampleMalformedSealed]))
  , case "conflicting_physical_segment_identity_is_invalid"
      (baseObservation
        (records := [sampleOpen, { sampleOpen with
          flush := some ⟨1, [⟨0, 1, none⟩], [67]⟩, createdAt := 6 }])
        (messages := []) (messageId := none))
  , case "raw_exact_record_replay_is_idempotent"
      (baseObservation (records := [sampleComplete, sampleComplete]))
  , case "fork_projection_uses_exact_child_session_and_origin_without_request_membership"
      { baseObservation (messages := [sampleMessage, sampleForkMessage])
          (messageId := some 201) with session := 2 }
  , case "missing_message_is_loading" (baseObservation (messages := []))
  , case "conflicting_message_identity_is_not_selected"
      (baseObservation (messages := [sampleMessage, { sampleMessage with nativeId := none }]))
  , case "missing_payload_dependency_is_loading" (baseObservation (records := []))
  , case "known_denial_is_not_loading" { baseObservation with deniedSegments := [100] }
  , case "current_generation_contiguous_open_source_is_live"
      (baseObservation (records := [sampleOpen]) (messages := []) (messageId := none))
  , case "stale_generation_open_source_is_not_live"
      (baseObservation (records := [sampleOpen]) (messages := []) (messageId := none)
        (owner := ⟨some (10, 8), []⟩))
  , case "missing_open_prefix_is_loading"
      (baseObservation (records := []) (messages := []) (messageId := none))
  , case "unreferenced_terminal_partial_is_retained_diagnostic"
      (baseObservation (records := [samplePartial]) (messages := []) (messageId := none)
        (owner := ⟨none, []⟩) (requestTerminal := true)
        (terminalSelection := some .noMessage))
  , case "partial_without_terminal_selection_is_loading"
      (baseObservation (records := [samplePartial]) (messages := []) (messageId := none)
        (owner := ⟨none, []⟩) (requestTerminal := true))
  , case "retracted_attempt_is_not_rendered"
      (baseObservation (records := [sampleRetracted]) (messages := []) (messageId := none)
        (owner := ⟨none, []⟩))
  , case "authored_without_header_is_not_live"
      (baseObservation (records := [{ sampleOpen with
          coordinate := ⟨10, .authored 4⟩ }]) (messages := []) (messageId := none)
        (coordinate := ⟨10, .authored 4⟩))
  , case "tool_partial_awaits_delivery"
      (baseObservation (records := [{ samplePartial with
          coordinate := ⟨10, .tool 42⟩, writer := .tool 42 }])
        (messages := []) (messageId := none) (coordinate := ⟨10, .tool 42⟩)
        (writer := .tool 42) (requestTerminal := true)
        (terminalSelection := some .noMessage))
  , case "header_before_payload_dependencies_is_loading"
      (baseObservation (records := []))
  , case "closed_source_is_never_a_live_preview"
      (baseObservation (messages := []) (messageId := none))
  , case "benign_late_record_beyond_extent_preserves_publication"
      (baseObservation
        (records := CanonicalOutput.deliver [sampleComplete] sampleLateBeyondExtent))
  , case "late_in_extent_twin_is_a_conflict"
      (baseObservation
        (records := CanonicalOutput.deliver [sampleComplete] sampleLateInExtentTwin))
  , case "valid_open_append_extends_live_preview"
      (baseObservation
        (records := CanonicalOutput.deliver [sampleOpen] sampleOpenContinuation)
        (messages := []) (messageId := none))
  , case "published_mixed_reasoning_renders_text_and_summary_only"
      mixedReasoningObservation
  , case "published_all_opaque_reasoning_renders_nothing"
      allOpaqueReasoningObservation
  ]

theorem outputProjectionCases_count : outputProjectionCases.length = 25 := by decide

theorem projection_cases_pin_boundaries :
    outputProjectionCases.map (fun witness => viewName witness.expected) =
       ["published", "published", "invalid", "invalid", "published", "published", "loading",
       "conflicted", "loading", "denied",
       "live", "absent", "loading", "retained_partial", "loading",
       "retracted", "absent", "settling", "loading", "settling", "published", "conflicted",
       "live", "published", "published"] := by
  native_decide

theorem published_presentation_excludes_opaque :
    (outputProjectionCases.map (fun witness => renderedKinds witness.expected)).head? =
      some ["text"] := by native_decide

theorem mixed_reasoning_presentation_excludes_opaque :
    renderedKinds (project mixedReasoningObservation) = ["reasoning", "summary"] := by
  native_decide

theorem all_opaque_reasoning_presentation_is_empty :
    renderedKinds (project allOpaqueReasoningObservation) = [] := by
  native_decide

theorem header_before_payload_dependencies_is_loading :
    project (baseObservation (records := [])) = .loading := by
  native_decide

def sampleWaitingClose : Segment :=
  { sampleComplete with flush := none, close := some (.closed .complete 2 [1, 1]) }

def sampleWrongWaitingClose : Segment :=
  { sampleWaitingClose with coordinate := ⟨11, .provider 0 0 0⟩ }

private def targetCase (name : String) (records : List Segment)
    (messages : List MessageEnvelope := []) (request : Nat := 10)
    (generation : Nat := 7) (inferenceScopes : List Nat := [0]) : TargetSelectionCase :=
  { name, request, generation, inferenceScopes, records, messages
    expected := selectTarget request generation inferenceScopes records messages }

def laterAttempt : Segment :=
  { sampleOpen with id := 120, coordinate := ⟨10, .provider 0 0 1⟩ }

def laterAttemptClose : Segment :=
  { laterAttempt with id := 121, flush := none, close := some (.closed .complete 1 [1, 1]) }

def laterAttemptMessage : MessageEnvelope :=
  { sampleMessage with
    header := { sampleMessage.header with id := 220, refs := [⟨121, 0⟩] }
    key := "later", sequence := 1 }

def conflictingLaterAttemptMessage : MessageEnvelope :=
  { laterAttemptMessage with
    header := { laterAttemptMessage.header with id := 221 }
    key := "later-conflict", sequence := 2 }

def laterInferenceScope : Segment :=
  { sampleOpen with id := 122, coordinate := ⟨10, .provider 2 0 0⟩ }

def staleWriterClose : Segment :=
  { laterAttemptClose with id := 123, writer := .request 8 }

def staleWriterMessage : MessageEnvelope :=
  { laterAttemptMessage with
    header := { laterAttemptMessage.header with id := 222, refs := [⟨123, 0⟩] }
    key := "stale-writer", sequence := 3 }

def targetSelectionCases : List TargetSelectionCase :=
  [ targetCase "target_absent_without_current_inference_source" []
  , targetCase "target_ignores_stale_generation"
      [{ sampleOpen with writer := .request 8 }]
  , targetCase "target_ignores_foreign_request"
      [{ sampleOpen with coordinate := ⟨11, .provider 0 0 0⟩ }]
  , targetCase "target_ignores_non_inference_scope"
      [{ sampleOpen with coordinate := ⟨10, .provider 1 0 0⟩ }]
  , targetCase "target_selects_latest_attempt_order_independently"
      [laterAttempt, sampleOpen]
  , targetCase "target_selects_latest_inference_scope"
      [sampleOpen, laterInferenceScope] (inferenceScopes := [0, 2])
  , targetCase "target_exact_replay_is_inert" [laterAttempt, laterAttempt]
  , targetCase "target_selects_unique_referencing_header"
      [laterAttempt, laterAttemptClose] [laterAttemptMessage]
  , targetCase "target_rejects_ambiguous_referencing_headers"
      [laterAttempt, laterAttemptClose]
      [laterAttemptMessage, conflictingLaterAttemptMessage]
  , targetCase "target_does_not_attach_stale_writer_close"
      [laterAttempt, staleWriterClose] [staleWriterMessage]
  ]

theorem targetSelectionCases_count : targetSelectionCases.length = 10 := by decide


theorem target_selection_boundaries :
    targetSelectionCases.map (fun witness => witness.expected) =
      [.absent, .absent, .absent, .absent,
       .selected ⟨laterAttempt.coordinate, laterAttempt.writer, none⟩,
       .selected ⟨laterInferenceScope.coordinate, laterInferenceScope.writer, none⟩,
       .selected ⟨laterAttempt.coordinate, laterAttempt.writer, none⟩,
       .selected ⟨laterAttempt.coordinate, laterAttempt.writer, some 220⟩,
       .conflicted,
       .selected ⟨laterAttempt.coordinate, laterAttempt.writer, none⟩] := by
  native_decide

theorem header_arrival_preserves_valid_open_prefix_as_settling :
    project (baseObservation (records := [sampleOpen, sampleWaitingClose])) =
      .settling [(sampleText, [65])] := by
  native_decide

theorem header_without_exact_target_close_does_not_attach_preview :
    project (baseObservation (records := [sampleOpen])) = .loading := by
  native_decide

theorem header_wrong_source_target_does_not_attach_preview :
    project (baseObservation (records := [sampleOpen, sampleWrongWaitingClose])) !=
      .settling [(sampleText, [65])] := by
  native_decide

theorem header_denied_target_prefix_is_denied :
    project { baseObservation (records := [sampleOpen, sampleWaitingClose]) with
      deniedSegments := [100] } = .denied := by
  native_decide

theorem same_session_different_id_same_key_is_conflicted :
    project { baseObservation with messages :=
      [sampleMessage, { sampleMessage with
        header := { sampleMessage.header with id := 299 }, sequence := 1 }] } = .conflicted := by
  native_decide

theorem same_session_different_id_same_sequence_is_conflicted :
    project { baseObservation with messages :=
      [sampleMessage, { sampleMessage with
        header := { sampleMessage.header with id := 298 }, key := "other" }] } = .conflicted := by
  native_decide

theorem closed_complete_source_is_not_live :
    project (baseObservation (messages := []) (messageId := none)) =
      .settling [(sampleText, [65])] := by
  native_decide

theorem closed_preview_ignores_malformed_data_beyond_committed_extent :
    project (baseObservation
      (records := [sampleComplete, { sampleLateBeyondExtent with writer := .request 99 }])
      (messages := []) (messageId := none)) = .settling [(sampleText, [65])] := by
  native_decide

/-- This deliberately does not claim arbitrary late records are harmless:
same-ID or in-extent twins remain conflicts. The witness is distinct, closureless
and outside the selected closure's committed extent. -/
theorem benign_late_record_beyond_extent_preserves_publication :
    project (baseObservation
      (records := CanonicalOutput.deliver [sampleComplete] sampleLateBeyondExtent)) =
      project baseObservation := by
  native_decide

theorem late_in_extent_twin_is_not_benign :
    project (baseObservation
      (records := CanonicalOutput.deliver [sampleComplete] sampleLateInExtentTwin)) =
      .conflicted := by
  native_decide

/-- Executable non-rewind regression for a valid contiguous append. This pins
the actual projected bytes, rather than inferring prefix safety merely from
record retention. -/
theorem valid_open_append_extends_live_preview :
    project (baseObservation (records := [sampleOpen])
      (messages := []) (messageId := none)) =
        .live [(sampleText, [65])] ∧
    project (baseObservation
      (records := CanonicalOutput.deliver [sampleOpen] sampleOpenContinuation)
      (messages := []) (messageId := none)) =
        .live [(sampleText, [65, 67])] ∧
    ([65] : List UInt8).IsPrefix [65, 67] := by
  native_decide

end StreamingResponse
