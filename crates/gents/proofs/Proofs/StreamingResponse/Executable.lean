import Proofs.StreamingResponse.Properties

namespace StreamingResponse

open CanonicalOutput

def sampleCoordinate : Coordinate := ⟨10, .provider 0 0 0⟩
def sampleText : Declaration := { block := 0, part := 0, kind := .text }
def sampleOpaque : Declaration := { block := 1, part := 0, kind := .opaque }

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

def viewName : View → String
  | .absent => "absent"
  | .live _ => "live"
  | .loading => "loading"
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
        | .opaque => none
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
  ]

theorem outputProjectionCases_count : outputProjectionCases.length = 14 := by decide

theorem projection_cases_pin_boundaries :
    outputProjectionCases.map (fun witness => viewName witness.expected) =
      ["published", "published", "loading", "conflicted", "loading", "denied",
       "live", "absent", "loading", "retained_partial", "loading",
       "retracted", "absent", "loading"] := by
  native_decide

theorem published_presentation_excludes_opaque :
    (outputProjectionCases.map (fun witness => renderedKinds witness.expected)).head? =
      some ["text"] := by native_decide

end StreamingResponse
