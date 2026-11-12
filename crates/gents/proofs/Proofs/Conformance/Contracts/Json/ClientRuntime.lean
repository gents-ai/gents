import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.Session.StreamingResponse.Executable
import Proofs.Session.StreamingResponse.ReasoningAudit
import Proofs.Session.StreamingResponse.ReasoningDeltas
import Proofs.Runtime.CanonicalOutput.Execution.AuxiliaryCases
import Proofs.Session.Compaction.Executable
import Proofs.Conformance.Recovery.ContractCases
import Proofs.Clients.QueuedSteering
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

namespace AgentDelivery

open Lean

deriving instance ToJson for QueuedSteering.AgentDelivery

structure Input where
  delivery : Option QueuedSteering.AgentDelivery
  interrupt : Option Bool
  deriving ToJson

end AgentDelivery

@[conformance_family]
def agentDelivery : Family AgentDelivery.Input (Option QueuedSteering.AgentDelivery) where
  inputs := QueuedSteering.agentDeliveryCases.map fun (delivery, interrupt) => { delivery, interrupt }
  model := fun i => QueuedSteering.resolveAgentDelivery i.delivery i.interrupt

def interruptDeliveryCaseJson (input : Option Nat × Option Nat) : String :=
  let number := fun value : Option Nat => match value with | none => "null" | some n => toString n
  "{\"planned\":" ++ number input.1 ++ ",\"active\":" ++ number input.2 ++ ",\"expected\":" ++
    number (QueuedSteering.interruptDeliveryTarget input.1 input.2) ++
    ",\"goal_paused\":" ++ boolString ((QueuedSteering.interruptDeliveryGoal true ⟨.active, 0, false, false⟩).status == .paused) ++ "}"

def agentSteerDispositionCaseJson (input : Bool × Bool) : String :=
  let result := match QueuedSteering.agentSteerDisposition input.1 input.2 with
    | .start => "start"
    | .steer => "steer"
    | .refuse => "refuse"
  "{\"active\":" ++ boolString input.1 ++ ",\"compatible\":" ++ boolString input.2 ++
    ",\"expected\":" ++ jsonString result ++ "}"

private def agentMessageOutcomeJson : QueuedSteering.AgentMessageOutcome → String
  | .pending => jsonString "pending"
  | .completed => jsonString "completed"
  | .failed => jsonString "failed"
  | .superseded => jsonString "superseded"

def agentMessageOutcomeCaseJson (input : Bool × QueuedSteering.AgentMessageOutcome × QueuedSteering.AgentMessageOutcome) : String :=
  "{\"folded\":" ++ boolString input.1 ++
    ",\"caused\":" ++ agentMessageOutcomeJson input.2.1 ++
    ",\"head\":" ++ agentMessageOutcomeJson input.2.2 ++
    ",\"expected\":" ++ agentMessageOutcomeJson (QueuedSteering.agentMessageOutcome input.1 input.2.1 input.2.2) ++ "}"

def foldedBindingCaseJson (input : QueuedSteering.FoldedBindingCase) : String :=
  let caused : QueuedSteering.FoldedScope := ⟨1, 2, some input.causedRequester, input.causedAgent⟩
  let head : QueuedSteering.FoldedScope := ⟨1, 2, some input.headRequester, input.headAgent⟩
  "{\"caused_requester\":" ++ toString input.causedRequester ++
    ",\"head_requester\":" ++ toString input.headRequester ++
    ",\"caused_agent\":" ++ toString input.causedAgent ++
    ",\"head_agent\":" ++ toString input.headAgent ++
    ",\"named_head\":" ++ toString input.namedHead ++
    ",\"actual_head\":" ++ toString input.actualHead ++
    ",\"expected\":" ++ boolString (QueuedSteering.foldedBindingValid caused head input.namedHead input.actualHead) ++ "}"

private def boundaryKindJson : QueuedSteering.BoundaryInputKind → String
  | .queued => jsonString "queued"
  | .userSteer => jsonString "user_steer"
  | .agentSteer => jsonString "agent_steer"
  | .blocked => jsonString "blocked"

def agentSteerTargetCaseJson (input : Nat × Nat) : String :=
  "{\"planned_head\":" ++ toString input.1 ++ ",\"active_head\":" ++ toString input.2 ++
    ",\"expected\":" ++ boolString (QueuedSteering.agentSteerTargets input.1 input.2) ++ "}"

def agentSteerReachabilityCaseJson (input : List (Nat × QueuedSteering.BoundaryInputKind × Bool) × Nat) : String :=
  let entryJson := fun entry => "{\"id\":" ++ toString entry.1 ++
    ",\"kind\":" ++ boundaryKindJson entry.2.1 ++ ",\"compatible\":" ++ boolString entry.2.2 ++ "}"
  "{\"inputs\":" ++ jsonArray (input.1.map entryJson) ++ ",\"target\":" ++ toString input.2 ++
    ",\"expected\":" ++ boolString (QueuedSteering.agentSteerReachable input.2 input.1) ++ "}"

namespace SteeringBoundary

open Lean

deriving instance ToJson for QueuedSteering.BoundaryInputKind

structure Entry where
  id : Nat
  kind : QueuedSteering.BoundaryInputKind
  deriving ToJson

structure Input where
  inputs : List Entry
  deriving ToJson

def ofCase (inputs : List (Nat × QueuedSteering.BoundaryInputKind)) : Input :=
  { inputs := inputs.map fun (id, kind) => { id, kind } }

def candidate (i : Input) : Option Nat :=
  QueuedSteering.boundaryCandidate (i.inputs.map fun e => (e.id, e.kind))

structure Publication where
  steered : List Nat
  pending : List Nat
  superseded : List Nat
  deriving ToJson

/-- A served fixture enqueues signed user turns behind an active head; an
agent steer needs a parent tool call and a blocked input a background
completion, which that fixture does not build. -/
def publicationCases : List (List (Nat × QueuedSteering.BoundaryInputKind)) :=
  QueuedSteering.boundaryCases.filter (· == [(1, .queued), (2, .userSteer), (3, .queued)])

end SteeringBoundary

@[conformance_family]
def steeringBoundarySelection : Family SteeringBoundary.Input (Option Nat) where
  inputs := QueuedSteering.boundaryCases.map SteeringBoundary.ofCase
  model := SteeringBoundary.candidate

@[conformance_family]
def steeringBoundaryPublication : Family SteeringBoundary.Input SteeringBoundary.Publication where
  inputs := SteeringBoundary.publicationCases.map SteeringBoundary.ofCase
  model := fun i =>
    let selected := SteeringBoundary.candidate i
    { steered := selected.toList
      pending := (i.inputs.filter fun e => some e.id != selected).map (·.id)
      superseded := selected.toList }

def liveOverlayCaseJson (witness : LiveOverlayCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"liveOutputAvailable\":" ++ boolString witness.liveOutputAvailable ++ ","
    ++ "\"hasDurableOwner\":" ++ boolString witness.hasDurableOwner ++ ","
    ++ "\"precedingToolCalls\":" ++ toString witness.precedingToolCalls ++ ","
    ++ "\"turnTerminal\":" ++ boolString witness.turnTerminal ++ ","
    ++ "\"turnLabel\":" ++ jsonString witness.turnLabel ++ ","
    ++ "\"hasContent\":" ++ boolString witness.hasContent ++ ","
    ++ "\"hasReasoning\":" ++ boolString witness.hasReasoning ++ ","
    ++ "\"expectOverlay\":" ++ boolString witness.expectOverlay
    ++ "}"

def requestProgressCaseJson (witness : RequestProgressCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"lifecycleState\":" ++ jsonString witness.lifecycleState ++ ","
    ++ "\"label\":" ++ jsonString witness.label ++ ","
    ++ "\"animated\":" ++ boolString witness.animated
    ++ "}"

namespace PendingUserTurn

open Lean

structure Input where
  name : String
  hasDurableUserOwner : Bool
  retiredByAuthor : Bool
  unrelatedUserTurns : Nat
  deriving ToJson

structure Output where
  pendingTurn : Bool
  deriving ToJson

end PendingUserTurn

@[conformance_family]
def pendingUserTurn : Family PendingUserTurn.Input PendingUserTurn.Output where
  inputs := pendingUserTurnCases.map fun c =>
    { name := c.name, hasDurableUserOwner := c.hasDurableUserOwner
      retiredByAuthor := c.retiredByAuthor, unrelatedUserTurns := c.unrelatedUserTurns }
  model := fun i => { pendingTurn := projectPendingUserTurn i.hasDurableUserOwner i.retiredByAuthor }

def queuedSteeringGuardJson (witness : QueuedSteering.GuardObservation) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"prefixAdmitted\":" ++ boolString witness.prefixAdmitted ++ ","
    ++ "\"admitted\":" ++ boolString witness.admitted
    ++ "}"

private def tagged (kind fields : String) : String :=
  "{\"kind\":" ++ jsonString kind ++ fields ++ "}"

private def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def byteArrayJson (bytes : List UInt8) : String :=
  jsonArray (bytes.map (fun byte => toString byte.toNat))

private def sourceJson : CanonicalOutput.Source → String
  | .provider scope turn attempt => tagged "provider"
      (",\"scope\":" ++ toString scope ++ ",\"turn\":" ++ toString turn ++
       ",\"attempt\":" ++ toString attempt)
  | .auxiliary kind scope turn attempt => tagged "auxiliary"
      (",\"auxiliary_kind\":" ++ jsonString (match kind with
        | .compaction => "compaction"
        | .compactionFallback => "compaction_fallback"
        | .title => "title"
          | .accessReview => "access_review") ++
       ",\"scope\":" ++ toString scope ++ ",\"turn\":" ++ toString turn ++
       ",\"attempt\":" ++ toString attempt)
  | .toolEffectArguments call => tagged "tool_effect_arguments" (",\"call\":" ++ toString call)
  | .tool call => tagged "tool" (",\"call\":" ++ toString call)
  | .authored key => tagged "authored" (",\"key\":" ++ toString key)

private def writerJson : CanonicalOutput.Writer → String
  | .request generation => tagged "request" (",\"generation\":" ++ toString generation)
  | .tool call => tagged "tool" (",\"call\":" ++ toString call)

private def outcomeJson : CanonicalOutput.Outcome → String
  | .complete => jsonString "complete"
  | .partial => jsonString "partial"

private def payloadKindJson : CanonicalOutput.PayloadKind → String
  | .text => "text" | .reasoning => "reasoning" | .summary => "summary"
  | .signature => "signature" | .reasoningStop => "reasoning_stop"
  | .encrypted => "encrypted" | .redacted => "redacted"
  | .arguments => "arguments" | .toolOutput => "tool_output"
  | .media => "media"

private def mediaKindJson : CanonicalOutput.MediaKind → String
  | .image => "image" | .audio => "audio" | .video => "video" | .document => "document"

private def optionalStringJson : Option String → String
  | none => "null" | some value => jsonString value

private def payloadRefJson (ref : CanonicalOutput.PayloadRef) : String :=
  "{\"close_id\":" ++ toString ref.closeId ++ ",\"stream\":" ++ toString ref.stream ++ "}"

def presentationJson : CanonicalOutput.Presentation → String
  | .full => tagged "full" ""
  | .composed parts => tagged "composed" (",\"parts\":" ++ jsonArray (parts.map fun part =>
      match part with
      | .range start stop => tagged "range"
          (",\"start\":" ++ toString start ++ ",\"end\":" ++ toString stop)
      | .literal bytes => tagged "literal" (",\"bytes\":" ++ byteArrayJson bytes)))

private def payloadSpecJson (spec : CanonicalOutput.PayloadSpec) : String :=
  "{\"reference\":" ++ payloadRefJson spec.reference ++
    ",\"presentation\":" ++ presentationJson spec.presentation ++ "}"

private def declarationJson (declaration : CanonicalOutput.Declaration) : String :=
  let tool := match declaration.tool with
    | none => "null"
    | some value => "{\"id\":" ++ jsonString value.id ++ ",\"call_id\":" ++
        optionalStringJson value.callId ++ ",\"name\":" ++ jsonString value.name ++ "}"
  "{\"block\":" ++ toString declaration.block ++ ",\"part\":" ++
    toString declaration.part ++ ",\"kind\":" ++ jsonString (payloadKindJson declaration.kind) ++
    ",\"tool\":" ++ tool ++ ",\"media_kind\":" ++
    (match declaration.mediaKind with | none => "null" | some kind => jsonString (mediaKindJson kind)) ++ "}"

private def runJson (run : CanonicalOutput.Run) : String :=
  "{\"stream\":" ++ toString run.stream ++ ",\"bytes\":" ++ toString run.bytes ++
    ",\"declaration\":" ++ (match run.declaration with
      | none => "null" | some declaration => declarationJson declaration) ++ "}"

private def flushJson (flush : CanonicalOutput.Flush) : String :=
  "{\"ordinal\":" ++ toString flush.ordinal ++ ",\"runs\":" ++
    jsonArray (flush.runs.map runJson) ++ ",\"payload\":" ++ byteArrayJson flush.payload ++ "}"

private def closureJson : CanonicalOutput.Closure → String
  | .retracted => tagged "retracted" ""
  | .closed outcome segments streamBytes => tagged "closed"
      (",\"outcome\":" ++ outcomeJson outcome ++ ",\"segments\":" ++ toString segments ++
       ",\"stream_bytes\":" ++ jsonArray (streamBytes.map toString))

def canonicalSegmentJson (segment : CanonicalOutput.Segment) : String :=
  "{\"id\":" ++ toString segment.id ++ ",\"coordinate\":{\"request\":" ++
    toString segment.coordinate.request ++ ",\"source\":" ++ sourceJson segment.coordinate.source ++
    "},\"writer\":" ++ writerJson segment.writer ++ ",\"flush\":" ++
    (match segment.flush with | none => "null" | some flush => flushJson flush) ++
    ",\"close\":" ++ (match segment.close with | none => "null" | some close => closureJson close) ++
    ",\"created_at\":" ++ toString segment.createdAt ++ "}"

private def roleJson : CanonicalOutput.MessageRole → String
  | .system => "system" | .user => "user" | .assistant => "assistant"

private def publicationJson : CanonicalOutput.MessagePublication → String
  | .requestExecution generation => tagged "request_execution" (",\"generation\":" ++ toString generation)
  | .requestRecovery generation => tagged "request_recovery" (",\"generation\":" ++ toString generation)
  | .toolDelivery call => tagged "tool_delivery" (",\"call\":" ++ toString call)
  | .fork origin => tagged "fork" (",\"origin\":" ++ toString origin)

private def reasoningPartJson {α : Type} (payloadJson : α → String) : CanonicalOutput.ReasoningPart α → String
  | .text payload signature => tagged "text" (",\"payload\":" ++ payloadJson payload ++
      ",\"signature\":" ++ optionalStringJson signature)
  | .encrypted payload => tagged "encrypted" (",\"payload\":" ++ payloadJson payload)
  | .redacted payload => tagged "redacted" (",\"payload\":" ++ payloadJson payload)
  | .summary payload => tagged "summary" (",\"payload\":" ++ payloadJson payload)

private def mediaJson {α : Type} (payloadJson : α → String)
    (media : CanonicalOutput.Media α) : String :=
  let data := match media.data with
    | .url url => tagged "url" (",\"url\":" ++ jsonString url)
    | .base64 payload => tagged "base64" (",\"payload\":" ++ payloadJson payload)
    | .raw payload => tagged "raw" (",\"payload\":" ++ payloadJson payload)
    | .string payload => tagged "string" (",\"payload\":" ++ payloadJson payload)
    | .unknown => tagged "unknown" ""
  "{\"kind\":" ++ jsonString (mediaKindJson media.kind) ++ ",\"data\":" ++ data ++
    ",\"media_type\":" ++ optionalStringJson media.mediaType ++
    ",\"detail\":" ++ optionalStringJson media.detail ++
    ",\"additional_params\":" ++ optionalStringJson media.additionalParams ++ "}"

private def resultPartJson {α : Type} (payloadJson : α → String) : CanonicalOutput.ResultPart α → String
  | .text payload => tagged "text" (",\"payload\":" ++ payloadJson payload)
  | .media value => tagged "media" (",\"value\":" ++ mediaJson payloadJson value)

private def messageBlockJson {α : Type} (payloadJson : α → String) : CanonicalOutput.MessageBlock α → String
  | .text payload => tagged "text" (",\"payload\":" ++ payloadJson payload)
  | .reasoning id parts => tagged "reasoning" (",\"id\":" ++ optionalStringJson id ++
      ",\"parts\":" ++ jsonArray (parts.map (reasoningPartJson payloadJson)))
  | .toolCall docId id callId name arguments signature additionalParams => tagged "tool_call"
      (",\"doc_id\":" ++ toString docId ++ ",\"id\":" ++ jsonString id ++
       ",\"call_id\":" ++ optionalStringJson callId ++ ",\"name\":" ++ jsonString name ++
       ",\"arguments\":" ++ payloadJson arguments ++ ",\"signature\":" ++ optionalStringJson signature ++
       ",\"additional_params\":" ++ optionalStringJson additionalParams)
  | .toolResult docId id callId parts => tagged "tool_result"
      (",\"doc_id\":" ++ toString docId ++ ",\"id\":" ++ jsonString id ++
       ",\"call_id\":" ++ optionalStringJson callId ++ ",\"parts\":" ++
       jsonArray (parts.map (resultPartJson payloadJson)))
  | .media value => tagged "media" (",\"value\":" ++ mediaJson payloadJson value)

def reconstructedMessageJson (native : CanonicalOutput.ReconstructedMessage) : String :=
  "{\"role\":" ++ jsonString (roleJson native.role) ++
    ",\"native_id\":" ++ optionalStringJson native.nativeId ++
    ",\"native_request_id\":" ++ optionalStringJson native.nativeRequestId ++
    ",\"blocks\":" ++ jsonArray (native.blocks.map (messageBlockJson byteArrayJson)) ++ "}"

def canonicalMessageJson (message : CanonicalOutput.MessageEnvelope) : String :=
  let h := message.header
  "{\"header\":{\"id\":" ++ toString h.id ++ ",\"session\":" ++ toString h.session ++
    ",\"request\":" ++ optionalNatJson h.request ++ ",\"origin\":" ++ optionalNatJson h.origin ++
    ",\"refs\":" ++ jsonArray (h.refs.map payloadRefJson) ++ ",\"outcome\":" ++ outcomeJson h.outcome ++
    ",\"role\":" ++ jsonString (roleJson h.role) ++ ",\"publication\":" ++ publicationJson h.publication ++
    "},\"key\":" ++ jsonString message.key ++ ",\"sequence\":" ++ toString message.sequence ++
    ",\"native_id\":" ++ optionalStringJson message.nativeId ++
    ",\"native_request_id\":" ++ optionalStringJson message.nativeRequestId ++ ",\"blocks\":" ++
    jsonArray (message.blocks.map (messageBlockJson payloadSpecJson)) ++
    ",\"created_at\":" ++ toString message.createdAt ++ "}"

def queuedSteeringActionName : QueuedSteering.Action → String
  | .enqueue => "enqueue"
  | .claimWithoutBegin => "claimWithoutBegin"
  | .claimAndBegin => "claimAndBegin"
  | .latchInterrupt => "latchInterrupt"
  | .terminate .interruptBeforeClaim => "interruptBeforeClaim"
  | .terminate .admissionReject => "admissionReject"
  | .terminate .failBeforeStream => "failBeforeStream"
  | .terminate .dedupLose => "dedupLose"
  | .terminate .expire => "expire"
  | .terminate .interruptClaimed => "interruptClaimed"
  | .terminate .interruptProcessing => "interruptProcessing"
  | .terminate .fail => "fail"
  | .terminate .finish => "finish"
  | .terminate .bindWorkspace => "bindWorkspace"
  | .terminate .claim => "claim"
  | .terminate .beginInference => "beginInference"
  | .terminate .continueProcessing => "continueProcessing"
  | .terminate (.parkBackend true) => "parkBackendClosed"
  | .terminate (.parkBackend false) => "parkBackendUnclosed"
  | .terminate (.resumeBackend true) => "resumeBackendReady"
  | .terminate (.resumeBackend false) => "resumeBackendNotReady"
  | .terminate .beginResumed => "beginResumed"
  | .terminate .interruptResuming => "interruptResuming"
  | .terminate .expireResuming => "expireResuming"
  | .terminate .interruptBackendWait => "interruptBackendWait"
  | .terminate .expireBackendWait => "expireBackendWait"
  | .publish => "publish"
  | .prepareFails => "prepareFails"
  | .capture => "capture"
  | .send => "send"

namespace QueuedSteeringTrace

open Lean CanonicalOutput

structure SourceWire where
  kind : String
  auxiliaryKind : Option String := none
  scope : Option Nat := none
  turn : Option Nat := none
  attempt : Option Nat := none
  call : Option Nat := none
  key : Option Nat := none
  deriving ToJson

def auxiliaryKindName : AuxiliaryKind → String
  | .compaction => "compaction"
  | .compactionFallback => "compaction_fallback"
  | .title => "title"
  | .accessReview => "access_review"

def SourceWire.ofModel : Source → SourceWire
  | .provider scope turn attempt =>
      { kind := "provider", scope := scope, turn := turn, attempt := attempt }
  | .auxiliary kind scope turn attempt =>
      { kind := "auxiliary", auxiliaryKind := auxiliaryKindName kind, scope := scope,
        turn := turn, attempt := attempt }
  | .toolEffectArguments call => { kind := "tool_effect_arguments", call := call }
  | .tool call => { kind := "tool", call := call }
  | .authored key => { kind := "authored", key := key }

structure CoordinateWire where
  request : Nat
  source : SourceWire
  deriving ToJson

structure WriterWire where
  kind : String
  generation : Option Nat := none
  call : Option Nat := none
  deriving ToJson

def WriterWire.ofModel : Writer → WriterWire
  | .request generation => { kind := "request", generation := generation }
  | .tool call => { kind := "tool", call := call }

def outcomeName : CanonicalOutput.Outcome → String
  | .complete => "complete"
  | .«partial» => "partial"

structure ToolWire where
  id : String
  callId : Option String
  name : String
  deriving ToJson

structure DeclarationWire where
  block : Nat
  part : Nat
  kind : String
  tool : Option ToolWire
  mediaKind : Option String
  deriving ToJson

def DeclarationWire.ofModel (declaration : CanonicalOutput.Declaration) : DeclarationWire :=
  { block := declaration.block, part := declaration.part,
    kind := payloadKindJson declaration.kind,
    tool := declaration.tool.map fun tool =>
      { id := tool.id, callId := tool.callId, name := tool.name },
    mediaKind := declaration.mediaKind.map mediaKindJson }

structure RunWire where
  stream : Nat
  bytes : Nat
  declaration : Option DeclarationWire
  deriving ToJson

structure FlushWire where
  ordinal : Nat
  runs : List RunWire
  payload : List Nat
  deriving ToJson

structure ClosureWire where
  kind : String
  outcome : Option String := none
  segments : Option Nat := none
  streamBytes : Option (List Nat) := none
  deriving ToJson

def ClosureWire.ofModel : Closure → ClosureWire
  | .retracted => { kind := "retracted" }
  | .closed outcome segments streamBytes =>
      { kind := "closed", outcome := outcomeName outcome, segments := segments,
        streamBytes := streamBytes }

structure SegmentWire where
  id : Nat
  coordinate : CoordinateWire
  writer : WriterWire
  flush : Option FlushWire
  close : Option ClosureWire
  createdAt : Nat
  deriving ToJson

def SegmentWire.ofModel (segment : Segment) : SegmentWire :=
  { id := segment.id,
    coordinate := { request := segment.coordinate.request,
                    source := SourceWire.ofModel segment.coordinate.source },
    writer := WriterWire.ofModel segment.writer,
    flush := segment.flush.map fun flush =>
      { ordinal := flush.ordinal,
        runs := flush.runs.map fun run =>
          { stream := run.stream, bytes := run.bytes,
            declaration := run.declaration.map DeclarationWire.ofModel },
        payload := flush.payload.map (·.toNat) },
    close := segment.close.map ClosureWire.ofModel,
    createdAt := segment.createdAt }

structure RefWire where
  closeId : Nat
  stream : Nat
  deriving ToJson

def RefWire.ofModel (ref : PayloadRef) : RefWire := { closeId := ref.closeId, stream := ref.stream }

structure PartWire where
  kind : String
  start : Option Nat := none
  stop : Option Nat := none
  bytes : Option (List Nat) := none
  deriving ToJson

structure PresentationWire where
  kind : String
  parts : Option (List PartWire) := none
  deriving ToJson

def PresentationWire.ofModel : Presentation → PresentationWire
  | .full => { kind := "full" }
  | .composed parts =>
      let wire : PresentationPart → PartWire
        | .range start stop => { kind := "range", start := start, stop := stop }
        | .literal bytes => { kind := "literal", bytes := bytes.map (·.toNat) }
      { kind := "composed", parts := some (parts.map wire) }

structure PayloadWire where
  reference : RefWire
  presentation : PresentationWire
  deriving ToJson

/-- A prepared steering input is one authored text block, so other block kinds
carry only their kind; `prepared_candidates_are_text` states it for every trace. -/
structure BlockWire where
  kind : String
  payload : Option PayloadWire
  deriving ToJson

def BlockWire.ofModel : MessageBlock PayloadSpec → BlockWire
  | .text payload =>
      let wire : PayloadWire :=
        { reference := RefWire.ofModel payload.reference
          presentation := PresentationWire.ofModel payload.presentation }
      { kind := "text", payload := some wire }
  | .reasoning _ _ => { kind := "reasoning", payload := none }
  | .toolCall .. => { kind := "tool_call", payload := none }
  | .toolResult .. => { kind := "tool_result", payload := none }
  | .media _ => { kind := "media", payload := none }

structure PublicationWire where
  kind : String
  generation : Option Nat := none
  call : Option Nat := none
  origin : Option Nat := none
  deriving ToJson

def PublicationWire.ofModel : MessagePublication → PublicationWire
  | .requestExecution generation => { kind := "request_execution", generation := generation }
  | .requestRecovery generation => { kind := "request_recovery", generation := generation }
  | .toolDelivery call => { kind := "tool_delivery", call := call }
  | .fork origin => { kind := "fork", origin := origin }

structure HeaderWire where
  id : Nat
  session : Nat
  request : Option Nat
  origin : Option Nat
  refs : List RefWire
  outcome : String
  role : String
  publication : PublicationWire
  deriving ToJson

structure MessageWire where
  header : HeaderWire
  key : String
  sequence : Nat
  nativeId : Option String
  nativeRequestId : Option String
  blocks : List BlockWire
  createdAt : Nat
  deriving ToJson

def MessageWire.ofModel (message : MessageEnvelope) : MessageWire :=
  let h := message.header
  { header := { id := h.id, session := h.session, request := h.request, origin := h.origin,
                refs := h.refs.map RefWire.ofModel, outcome := outcomeName h.outcome,
                role := roleJson h.role, publication := PublicationWire.ofModel h.publication },
    key := message.key, sequence := message.sequence, nativeId := message.nativeId,
    nativeRequestId := message.nativeRequestId, blocks := message.blocks.map BlockWire.ofModel,
    createdAt := message.createdAt }

structure CandidateWire where
  closing : SegmentWire
  message : MessageWire
  deriving ToJson

structure EntryWire where
  requestId : Nat
  createdAt : Nat
  source : String
  policy : String
  queueKey : Option Nat
  queuedAfter : Option Nat
  deriving ToJson

structure CaptureWire where
  nodeDid : Nat
  sessionId : Nat
  requestDocId : Nat
  turnIndex : Nat
  attempt : Nat
  bodyToken : Nat
  priorBodyToken : Option Nat
  deriving ToJson

structure ObservationWire where
  requestId : Nat
  requestDocId : Nat
  entry : EntryWire
  interruptAt : Option Nat
  preparedCandidate : Option CandidateWire
  capture : CaptureWire
  lifecycleState : String
  acceptedInput : Bool
  queueActive : Option Nat
  admissionVisible : Bool
  canonicalAuthoredCount : Nat
  providerSendPermitted : Bool
  deriving ToJson

def ObservationWire.ofModel (witness : QueuedSteering.TraceObservation) : ObservationWire :=
  let script := witness.caseScript
  let key := script.capture.key
  { requestId := witness.requestId, requestDocId := witness.requestDocId,
    entry := { requestId := script.entry.requestId, createdAt := script.entry.createdAt,
               source := script.entry.source.toDefraDB, policy := script.entry.policy.toDefraDB,
               queueKey := script.entry.queueKey, queuedAfter := script.entry.queuedAfter },
    interruptAt := script.interruptAt,
    preparedCandidate := script.candidate.map fun prepared =>
      { closing := SegmentWire.ofModel prepared.closing,
        message := MessageWire.ofModel prepared.message },
    capture := { nodeDid := key.nodeDid, sessionId := key.sessionId, requestDocId := key.requestId,
                 turnIndex := key.turnIndex, attempt := key.attempt,
                 bodyToken := script.capture.request.value,
                 priorBodyToken := script.capture.priorBinding.map (·.value) },
    lifecycleState := witness.lifecycleState, acceptedInput := witness.acceptedInput,
    queueActive := witness.queueActive, admissionVisible := witness.admissionVisible,
    canonicalAuthoredCount := witness.canonicalAuthoredCount,
    providerSendPermitted := witness.providerSendPermitted }

def actionNames (witness : QueuedSteering.TraceObservation) : List String :=
  witness.caseScript.actions.map queuedSteeringActionName

structure ScriptInput where
  name : String
  actions : List String
  contentToken : Nat
  deriving ToJson

def ScriptInput.ofModel (witness : QueuedSteering.TraceObservation) : ScriptInput :=
  { name := witness.name, actions := actionNames witness, contentToken := witness.contentToken }

def observationNamed (name : String) : Option QueuedSteering.TraceObservation :=
  QueuedSteering.traceObservations.find? (·.name == name)

theorem trace_names_are_unique :
    (QueuedSteering.traceObservations.map (·.name)).eraseDups.length =
      QueuedSteering.traceObservations.length := by
  native_decide

def tracesWithActions (scripts : List (List String)) : List QueuedSteering.TraceObservation :=
  QueuedSteering.traceObservations.filter fun witness => actionNames witness ∈ scripts

structure PendingOutput where
  lifecycleState : String
  acceptedInput : Bool
  canonicalAuthoredCount : Nat
  deriving ToJson

/-- A pending steering request reaches a terminal without a claim only by an
interrupt before its claim or an admission rejection; the served scenario
enqueues a production-signed request and drives one of the two. -/
@[conformance_family]
def pendingTerminal : Family ScriptInput (Option PendingOutput) where
  inputs := (tracesWithActions [["enqueue", "interruptBeforeClaim"], ["enqueue", "admissionReject"]]).map
    ScriptInput.ofModel
  model := fun i => (observationNamed i.name).map fun witness =>
    { lifecycleState := witness.lifecycleState, acceptedInput := witness.acceptedInput,
      canonicalAuthoredCount := witness.canonicalAuthoredCount }

structure OwnedOutput where
  lifecycleState : String
  acceptedInput : Bool
  canonicalAuthoredCount : Nat
  queueActive : Bool
  deriving ToJson

/-- The served owned scenario claims and begins a production-signed steering
request and terminalizes it before any authored publication, by failure or by
a latched interrupt; it executes no context preparation or provider dispatch. -/
@[conformance_family]
def ownedTerminal : Family ScriptInput (Option OwnedOutput) where
  inputs := (tracesWithActions
    [["enqueue", "claimAndBegin", "prepareFails", "fail", "capture", "send"],
     ["enqueue", "claimAndBegin", "prepareFails", "latchInterrupt", "interruptProcessing",
      "capture", "send"]]).map ScriptInput.ofModel
  model := fun i => (observationNamed i.name).map fun witness =>
    { lifecycleState := witness.lifecycleState, acceptedInput := witness.acceptedInput,
      canonicalAuthoredCount := witness.canonicalAuthoredCount,
      queueActive := witness.queueActive.isSome }

structure PublicationInput where
  name : String
  actions : List String
  prompt : String
  deriving ToJson

structure PublicationOutput where
  canonicalAuthoredCount : Nat
  providerSendPermitted : Bool
  deriving ToJson

def preparedPrompt (witness : QueuedSteering.TraceObservation) : String :=
  ((witness.caseScript.candidate.bind (·.closing.flush)).bind fun flush =>
    String.fromUTF8? (ByteArray.mk flush.payload.toArray)).getD ""

/-- The owned loop publishes the prepared input once and enters the provider
stream; a conflicting capture binding has no loop fixture, so only the
publication handoff is replayed there. -/
@[conformance_family]
def publication : Family PublicationInput (Option PublicationOutput) where
  inputs := (QueuedSteering.traceObservations.filter fun witness =>
      witness.name == "queued_steering_publishes_once_only_after_owned_execution_start").map
    fun witness => { name := witness.name, actions := actionNames witness,
                     prompt := preparedPrompt witness }
  model := fun i => (observationNamed i.name).map fun witness =>
    { canonicalAuthoredCount := witness.canonicalAuthoredCount,
      providerSendPermitted := witness.providerSendPermitted }

/-- model-production-gap: no native owner exposes the whole steering trace
observation (admission visibility, capture binding, queue head and prepared
candidate); claimed pre-start failure, prepublication send denial, conflicting
capture and exact replay scripts have no native replay. -/
@[conformance_family]
def traceUnreached : Family ScriptInput (Option ObservationWire) where
  inputs := QueuedSteering.traceObservations.map ScriptInput.ofModel
  model := fun i => (observationNamed i.name).map ObservationWire.ofModel

def pendingFacts (witness : QueuedSteering.TraceObservation) : Bool :=
  let entry := witness.caseScript.entry
  entry.requestId == witness.requestId && entry.source.toDefraDB == "steering" &&
    entry.policy.toDefraDB == "append" && entry.queueKey.isNone && entry.queuedAfter.isSome &&
    (actionNames witness != ["enqueue", "interruptBeforeClaim"] || witness.caseScript.interruptAt.isSome)

example : (tracesWithActions [["enqueue", "interruptBeforeClaim"], ["enqueue", "admissionReject"]]).all
    pendingFacts := by
  native_decide

def ownedFacts (witness : QueuedSteering.TraceObservation) : Bool :=
  witness.acceptedInput && !witness.providerSendPermitted && witness.canonicalAuthoredCount == 0 &&
    witness.queueActive.isNone && witness.caseScript.interruptAt.isNone

example : (tracesWithActions
    [["enqueue", "claimAndBegin", "prepareFails", "fail", "capture", "send"],
     ["enqueue", "claimAndBegin", "prepareFails", "latchInterrupt", "interruptProcessing",
      "capture", "send"]]).all ownedFacts := by
  native_decide

def connectedFacts (witness : QueuedSteering.TraceObservation) : Bool :=
  witness.requestId == witness.caseScript.entry.requestId &&
    witness.caseScript.capture.key.requestId == witness.requestDocId &&
    !witness.caseScript.actions.isEmpty && witness.acceptedInput &&
    (!(witness.lifecycleState == "failed" || witness.lifecycleState == "interrupted") ||
      witness.queueActive.isNone)

example : QueuedSteering.traceObservations.length = 9 ∧
    QueuedSteering.traceObservations.all connectedFacts := by
  native_decide

def candidateFacts (witness : QueuedSteering.TraceObservation) : Bool :=
  match witness.caseScript.candidate with
  | none => false
  | some prepared =>
      let segment := prepared.closing
      let message := prepared.message
      match segment.coordinate.source, segment.writer, message.header.publication,
          message.blocks, segment.flush with
      | .authored _, .request generation, .requestExecution owner, [.text payload], some flush =>
          match flush.runs with
          | [run] =>
              segment.coordinate.request == witness.requestDocId && owner == generation &&
                message.header.request == some witness.requestDocId &&
                message.header.role == .user && message.header.outcome == .complete &&
                payload.presentation == .full && payload.reference.closeId == segment.id &&
                payload.reference.stream == 0 && message.header.refs == [payload.reference] &&
                flush.ordinal == 0 && run.stream == payload.reference.stream &&
                run.bytes == flush.payload.length &&
                (run.declaration.map fun d => d.block == 0 && d.part == 0 && d.kind == .text) == some true &&
                segment.close == some (.closed .complete 1 [run.bytes])
          | _ => false
      | _, _, _, _, _ => false

example : (QueuedSteering.traceObservations.filter fun witness =>
      witness.name == "queued_steering_publishes_once_only_after_owned_execution_start").all
    candidateFacts := by
  native_decide

theorem prepared_candidates_are_text :
    QueuedSteering.traceObservations.all (fun witness =>
      (witness.caseScript.candidate.map fun prepared =>
        prepared.message.blocks.all fun block =>
          match block with
          | .text _ => true
          | _ => false).getD true) = true := by
  native_decide

end QueuedSteeringTrace

private def streamsJson (streams : CanonicalOutput.Streams) : String :=
  jsonArray (streams.map fun stream => "{\"declaration\":" ++ declarationJson stream.1 ++
    ",\"bytes\":" ++ byteArrayJson stream.2 ++ "}")

private def outputObservationJson (observation : StreamingResponse.Observation) : String :=
  "{\"request\":" ++ toString observation.request ++ ",\"session\":" ++ toString observation.session ++
    ",\"records\":" ++ jsonArray (observation.records.map canonicalSegmentJson) ++
    ",\"messages\":" ++ jsonArray (observation.messages.map canonicalMessageJson) ++
    ",\"denied_headers\":" ++ jsonArray (observation.deniedHeaders.map toString) ++
    ",\"denied_segments\":" ++ jsonArray (observation.deniedSegments.map toString) ++
    ",\"dependency_denials\":" ++ jsonArray (observation.dependencyDenials.map fun denial =>
      "{\"root_close_id\":" ++ toString denial.rootCloseId ++
       ",\"denied_doc_id\":" ++ toString denial.deniedDocId ++ "}") ++
    ",\"owner\":{\"current_request\":" ++ (match observation.owner.currentRequest with
      | none => "null" | some value => "[" ++ toString value.1 ++ "," ++ toString value.2 ++ "]") ++
    ",\"live_tools\":" ++ jsonArray (observation.owner.liveTools.map toString) ++ "}," ++
    "\"target\":{\"coordinate\":{\"request\":" ++ toString observation.target.coordinate.request ++
    ",\"source\":" ++ sourceJson observation.target.coordinate.source ++ "},\"writer\":" ++
    writerJson observation.target.writer ++ ",\"message_id\":" ++ optionalNatJson observation.target.messageId ++ "}," ++
    "\"request_terminal\":" ++ boolString observation.requestTerminal ++
    ",\"terminal_selection\":" ++ (match observation.terminalSelection with
      | none => "null" | some .noMessage => tagged "no_message" ""
      | some (.message id) => tagged "message" (",\"id\":" ++ toString id)) ++ "}"

private def outputViewJson : StreamingResponse.View → String
  | .absent => tagged "absent" "" | .loading => tagged "loading" ""
  | .denied => tagged "denied" "" | .conflicted => tagged "conflicted" ""
  | .invalid => tagged "invalid" "" | .retracted => tagged "retracted" ""
  | .live streams => tagged "live" (",\"streams\":" ++ streamsJson streams)
  | .settling streams => tagged "settling" (",\"streams\":" ++ streamsJson streams)
  | .retainedPartial streams => tagged "retained_partial" (",\"streams\":" ++ streamsJson streams)
  | .published message native => tagged "published"
      (",\"message\":" ++ canonicalMessageJson message ++
       ",\"native\":" ++ reconstructedMessageJson native)

def outputProjectionCaseJson
    (witness : StreamingResponse.OutputProjectionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"input\":" ++ outputObservationJson witness.input ++ ","
    ++ "\"expected\":" ++ outputViewJson witness.expected ++ ","
    ++ "\"rendered_kinds\":" ++ jsonArray
      ((StreamingResponse.renderedKinds witness.expected).map jsonString)
    ++ "}"

def reasoningAuditCaseJson (witness : StreamingResponse.ReasoningAudit.AuditCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"input\":" ++ outputObservationJson witness.input ++
    ",\"expected\":" ++ (match witness.expected with
      | .ok streams => tagged "ok" (",\"streams\":" ++ streamsJson streams)
      | .error .loading => tagged "loading" ""
      | .error .conflicted => tagged "conflicted" ""
      | .error .invalid => tagged "invalid" "") ++ "}"

def reasoningSignatureCaseJson
    (witness : StreamingResponse.ReasoningAudit.SignatureCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"records\":" ++ jsonArray (witness.records.map canonicalSegmentJson) ++
    ",\"payload\":" ++ payloadSpecJson witness.payload ++
    ",\"signature\":" ++ optionalStringJson witness.signature ++
    ",\"accepted\":" ++ boolString witness.accepted ++ "}"

def reasoningDeltaPartJson : StreamingResponse.ReasoningDeltas.Part → String
  | .reasoning id signature bytes =>
      "{\"kind\":\"reasoning\",\"id\":" ++ optionalNatJson id ++
      ",\"signature\":" ++ optionalNatJson signature ++
      ",\"bytes\":" ++ jsonArray (bytes.map toString) ++ "}"
  | .text bytes =>
      "{\"kind\":\"text\",\"bytes\":" ++ jsonArray (bytes.map toString) ++ "}"

def reasoningDeltaCaseJson (witness : StreamingResponse.ReasoningDeltas.Case) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"before\":" ++ jsonArray (witness.before.map reasoningDeltaPartJson) ++
    ",\"id\":" ++ optionalNatJson witness.id ++
    ",\"fragment\":" ++ jsonArray (witness.fragment.map toString) ++
    ",\"expected\":" ++ jsonArray (witness.expected.map reasoningDeltaPartJson) ++ "}"

def auxiliaryOutputCaseJson
    (witness : CanonicalOutput.Execution.AuxiliaryCases.Case) : String :=
  let kind := witness.kind
  let raw := CanonicalOutput.Execution.AuxiliaryCases.observed kind
  let closing := CanonicalOutput.Execution.AuxiliaryCases.close kind witness.outcome
  let staleClosing := CanonicalOutput.Execution.AuxiliaryCases.staleWriterClose kind witness.outcome
  let claimed := CanonicalOutput.Execution.AuxiliaryCases.claimedWorld
  let publicationMessage := CanonicalOutput.Execution.Examples.emptyAssistant 200 5
  let nativePublicationClosing := CanonicalOutput.Execution.AuxiliaryCases.nativePublicationClose kind
  let nativePublicationMessage := CanonicalOutput.Execution.AuxiliaryCases.nativePublicationMessage
  let recoveryClosing := CanonicalOutput.Execution.AuxiliaryCases.recoveryClose kind
  let observation := CanonicalOutput.Execution.AuxiliaryCases.observation kind [raw]
  let recovery := CanonicalOutput.Execution.recoverExpiredBatch
    (CanonicalOutput.Execution.Examples.world 20 [raw]) 7 8 5 30
    [⟨recoveryClosing, none⟩]
  let recoveryWithHeader := CanonicalOutput.Execution.recoverExpiredBatch
    (CanonicalOutput.Execution.Examples.world 20 [raw]) 7 8 5 30
    [⟨recoveryClosing, some (CanonicalOutput.Execution.Examples.recoveryMessage 201 101 0 20)⟩]
  let publication := CanonicalOutput.Execution.acceptAndPublish
    (CanonicalOutput.Execution.Examples.world 5 [raw]) 7 closing
    publicationMessage []
  "{\"name\":" ++ jsonString witness.name ++
    ",\"raw\":" ++ canonicalSegmentJson raw ++
    ",\"closing\":" ++ canonicalSegmentJson closing ++
    ",\"stale_closing\":" ++ canonicalSegmentJson staleClosing ++
    ",\"authority\":{\"claimed_request_state\":" ++
      jsonString claimed.lease.request.toDefraDB ++
      ",\"generation\":" ++ optionalNatJson claimed.currentGeneration? ++
      ",\"begin_boundary\":\"mutation_write_gate\"}" ++
    ",\"publication_message\":" ++ canonicalMessageJson publicationMessage ++
    ",\"native_publication_closing\":" ++ canonicalSegmentJson nativePublicationClosing ++
    ",\"native_publication_message\":" ++ canonicalMessageJson nativePublicationMessage ++
    ",\"recovery_closing\":" ++ canonicalSegmentJson recoveryClosing ++
    ",\"observation\":" ++ outputObservationJson observation ++
    ",\"expected\":{\"append_accepted\":" ++ boolString
      (CanonicalOutput.Execution.Examples.succeeds
        (CanonicalOutput.Execution.appendRaw
          (CanonicalOutput.Execution.Examples.world 5) 7 raw)) ++
    ",\"close_accepted\":" ++ boolString
      (CanonicalOutput.Execution.Examples.succeeds
        (CanonicalOutput.Execution.closeAuxiliary
          (CanonicalOutput.Execution.Examples.world 5 [raw]) 7 closing)) ++
    ",\"claimed_append_accepted\":" ++ boolString
      (CanonicalOutput.Execution.AuxiliaryCases.claimedAppendAccepted kind) ++
    ",\"begin_accepted\":" ++ boolString
      CanonicalOutput.Execution.AuxiliaryCases.begunWorld?.isSome ++
    ",\"begun_append_accepted\":" ++ boolString
      (CanonicalOutput.Execution.AuxiliaryCases.begunAppendAccepted kind) ++
    ",\"stale_close_replay_accepted\":" ++ boolString
      (CanonicalOutput.Execution.AuxiliaryCases.staleWriterCloseReplayAccepted
        kind witness.outcome) ++
    ",\"empty_header_accepted\":" ++ boolString
      (CanonicalOutput.Execution.AuxiliaryCases.emptyHeaderAccepted kind witness.outcome) ++
    ",\"native_publication_accepted\":" ++ boolString
      (CanonicalOutput.Execution.AuxiliaryCases.nativePublicationAccepted kind) ++
    ",\"recovery_accepted\":" ++ boolString
      (CanonicalOutput.Execution.Examples.succeeds recovery) ++
    ",\"recovery_with_header_accepted\":" ++ boolString
      (CanonicalOutput.Execution.Examples.succeeds recoveryWithHeader) ++
    ",\"publication_accepted\":" ++ boolString
      (CanonicalOutput.Execution.Examples.succeeds publication) ++
    ",\"audit\":" ++ (match StreamingResponse.reconstructAuditPrefix observation none with
      | .ok streams => tagged "ok" (",\"streams\":" ++ streamsJson streams)
      | .error .loading => tagged "loading" ""
      | .error .conflicted => tagged "conflicted" ""
      | .error .invalid => tagged "invalid" "") ++
    ",\"public_view\":" ++ outputViewJson (StreamingResponse.project observation) ++
    "}}"

def compactionInputRowJson (row : Transcript.MessageRow) : String :=
  let role := match row.role with | .user => "user" | .assistant => "assistant"
  let body := match row.kind with
    | .ordinary => "\"kind\":\"ordinary\",\"text\":" ++ jsonString s!"ordinary-{row.messageId}"
    | .assistantToolCalls calls =>
        "\"kind\":\"assistant_tool_calls\",\"call_ids\":" ++
          jsonArray ((calls.sort (· ≤ ·)).map (fun call => jsonString (Compaction.fixtureNativeCallKey call)))
    | .toolResult call _ =>
        "\"kind\":\"tool_result\",\"call_id\":" ++ jsonString (Compaction.fixtureNativeCallKey call)
  "{\"role\":" ++ jsonString role ++ "," ++ body ++ "}"

def compactionReducerCaseJson (witness : Compaction.CompactionReducerCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"reducer\":" ++ jsonString witness.reducer ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"input_rows\":" ++ jsonArray (witness.inputRows.map compactionInputRowJson) ++ ","
    ++ "\"projected_prefix_ready\":" ++ boolString witness.projectedPrefixReady ++ ","
    ++ "\"pre_message_count\":" ++ toString witness.preMessageCount ++ ","
    ++ "\"post_message_count\":" ++ toString witness.postMessageCount ++ ","
    ++ "\"preserves_pairs\":" ++ boolString witness.preservesPairs ++ ","
    ++ "\"preserves_order\":" ++ boolString witness.preservesOrder ++ ","
    ++ "\"gate_open\":" ++ jsonOptionalBool witness.gateOpen ++ ","
    ++ "\"publication_ready\":" ++ boolString witness.publicationReady ++ ","
    ++ "\"provider_fixpoint\":" ++ boolString witness.providerFixpoint ++ ","
    ++ "\"turn_boundary\":" ++ boolString witness.turnBoundary ++ ","
    ++ "\"safe_to_reduce\":" ++ boolString witness.safeToReduce ++ ","
    ++ "\"reducer_is_identity\":"
      ++ boolString witness.reducerIsIdentity ++ ","
    ++ "\"reducer_is_idempotent\":"
      ++ boolString witness.reducerIsIdempotent ++ ","
    ++ "\"split_index\":" ++ toString witness.splitIndex ++ ","
    ++ "\"safe_boundary\":" ++ toString witness.safeBoundary ++ ","
    ++ "\"retained_count\":" ++ toString witness.retainedCount
    ++ "}"

def compactionCursorCaseJson (witness : Compaction.CompactionCursorCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"compacted\":" ++ toString witness.compacted ++ ","
    ++ "\"expected_cursor\":" ++ jsonOptionalNat witness.expectedCursor
    ++ "}"

def recoverySweepCaseJson (witness : RecoverySweepCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"sweep_id\":" ++ jsonString witness.sweepId ++ ","
    ++ "\"collection\":" ++ jsonString witness.collection ++ ","
    ++ "\"rust_function\":" ++ jsonString witness.rustFunction ++ ","
    ++ "\"cadence\":" ++ jsonString witness.cadence ++ ","
    ++ "\"implementation_status\":"
      ++ jsonString witness.implementationStatus ++ ","
    ++ "\"pre_state\":" ++ jsonString witness.preState ++ ","
    ++ "\"terminal_state\":" ++ jsonString witness.terminalState ++ ","
    ++ "\"measure_before\":" ++ toString witness.measureBefore ++ ","
    ++ "\"measure_after\":" ++ toString witness.measureAfter ++ ","
    ++ "\"deadline_expired\":" ++ jsonOptionalBool witness.deadlineExpired ++ ","
    ++ "\"parent_live\":" ++ jsonOptionalBool witness.parentLive ++ ","
    ++ "\"parent_interrupted\":" ++ jsonOptionalBool witness.parentInterrupted ++ ","
    ++ "\"parent_terminal\":" ++ jsonOptionalBool witness.parentTerminal ++ ","
    ++ "\"task_hook_observation\":" ++ jsonOptionalString witness.taskHookObservation ++ ","
    ++ "\"execution_registered\":"
      ++ jsonOptionalBool witness.executionRegistered ++ ","
    ++ "\"process_outcome\":" ++ jsonOptionalString witness.processOutcome ++ ","
    ++ "\"owner_task_deleted\":"
      ++ jsonOptionalBool witness.ownerTaskDeleted ++ ","
    ++ "\"owner_session_stopped\":"
      ++ jsonOptionalBool witness.ownerSessionStopped ++ ","
    ++ "\"recovery_cause\":" ++ jsonOptionalString witness.recoveryCause ++ ","
    ++ "\"notification_reason\":"
      ++ jsonOptionalString witness.notificationReason ++ ","
    ++ "\"deadline_audit_ref\":"
    ++ jsonString witness.deadlineAuditRef
    ++ "}"

def restartDispositionCaseJson (witness : RestartDispositionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"rust_function\":" ++ jsonString witness.rustFunction ++ ","
    ++ "\"await_mode\":" ++ jsonString witness.awaitMode ++ ","
    ++ "\"session_message\":"
      ++ boolString (witness.kind == .sessionMessage) ++ ","
    ++ "\"kind\":"
      ++ jsonString (match witness.kind with
        | .native => "native"
        | .sessionMessage => "sessionMessage"
        | .pluginJob => "pluginJob") ++ ","
    ++ "\"parent_observation\":"
      ++ jsonString witness.parentObservation ++ ","
    ++ "\"deadline_expired\":" ++ boolString witness.deadlineExpired ++ ","
    ++ "\"process_outcome\":" ++ jsonString witness.processOutcome ++ ","
    ++ "\"disposition\":" ++ jsonString witness.disposition ++ ","
    ++ "\"cause\":" ++ jsonOptionalString witness.cause ++ ","
    ++ "\"terminal_state\":" ++ jsonOptionalString witness.terminalState ++ ","
    ++ "\"notification_reason\":"
      ++ jsonOptionalString witness.notificationReason ++ ","
    ++ "\"queue_source\":" ++ jsonOptionalString witness.queueSource ++ ","
    ++ "\"queue_key_prefix\":"
      ++ jsonOptionalString witness.queueKeyPrefix ++ ","
    ++ "\"theorem\":" ++ jsonString witness.theoremName
    ++ "}"

end Conformance.Contracts
