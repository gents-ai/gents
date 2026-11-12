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

private def agentDeliveryJson : Option QueuedSteering.AgentDelivery → String
  | none => "null"
  | some .queue => jsonString "queue"
  | some .steer => jsonString "steer"
  | some .interrupt => jsonString "interrupt"

def agentDeliveryCaseJson (input : Option QueuedSteering.AgentDelivery × Option Bool) : String :=
  "{\"delivery\":" ++ agentDeliveryJson input.1 ++
    ",\"interrupt\":" ++ (match input.2 with | none => "null" | some b => boolString b) ++
    ",\"expected\":" ++ agentDeliveryJson (QueuedSteering.resolveAgentDelivery input.1 input.2) ++ "}"

def interruptDeliveryCaseJson (input : Option Nat × Option Nat) : String :=
  let number := fun value : Option Nat => match value with | none => "null" | some n => toString n
  "{\"planned\":" ++ number input.1 ++ ",\"active\":" ++ number input.2 ++ ",\"expected\":" ++
    number (QueuedSteering.interruptDeliveryTarget input.1 input.2) ++
    ",\"goal_paused\":" ++ boolString ((QueuedSteering.interruptDeliveryGoal true ⟨.active, 0, false, false⟩).status == .paused) ++ "}"

deriving instance Lean.ToJson for QueuedSteering.AgentSteerDisposition,
  QueuedSteering.BoundaryInputKind

structure AgentSteerDispositionInput where
  active : Bool
  compatible : Bool
  deriving Lean.ToJson

@[conformance_family]
def agentSteerDisposition :
    Family AgentSteerDispositionInput QueuedSteering.AgentSteerDisposition where
  inputs := QueuedSteering.agentSteerDispositionCases.map fun (active, compatible) =>
    { active, compatible }
  model := fun i => QueuedSteering.agentSteerDisposition i.active i.compatible

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

structure AgentSteerTargetInput where
  plannedHead : Nat
  activeHead : Nat
  deriving Lean.ToJson

@[conformance_family]
def agentSteerTarget : Family AgentSteerTargetInput Bool where
  inputs := QueuedSteering.agentSteerTargetCases.map fun (plannedHead, activeHead) =>
    { plannedHead, activeHead }
  model := fun i => QueuedSteering.agentSteerTargets i.plannedHead i.activeHead

structure AgentSteerEntry where
  id : Nat
  kind : QueuedSteering.BoundaryInputKind
  compatible : Bool
  deriving Lean.ToJson

structure AgentSteerReachabilityInput where
  inputs : List AgentSteerEntry
  target : Nat
  deriving Lean.ToJson

@[conformance_family]
def agentSteerReachability : Family AgentSteerReachabilityInput Bool where
  inputs := QueuedSteering.agentSteerReachabilityCases.map fun (entries, target) =>
    { inputs := entries.map fun (id, kind, compatible) => { id, kind, compatible }, target }
  model := fun i =>
    QueuedSteering.agentSteerReachable i.target (i.inputs.map fun e => (e.id, e.kind, e.compatible))

def boundaryCaseJson (inputs : List (Nat × QueuedSteering.BoundaryInputKind)) : String :=
  let entryJson := fun entry => "{\"id\":" ++ toString entry.1 ++
    ",\"kind\":" ++ boundaryKindJson entry.2 ++ "}"
  let expected := match QueuedSteering.boundaryCandidate inputs with
    | none => "null"
    | some id => toString id
  "{\"inputs\":" ++ jsonArray (inputs.map entryJson) ++ ",\"expected\":" ++ expected ++ "}"

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

def pendingUserTurnCaseJson (witness : PendingUserTurnCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"hasDurableUserOwner\":"
      ++ boolString witness.hasDurableUserOwner ++ ","
    ++ "\"retiredByAuthor\":" ++ boolString witness.retiredByAuthor ++ ","
    ++ "\"unrelatedUserTurns\":" ++ toString witness.unrelatedUserTurns ++ ","
    ++ "\"expectPendingTurn\":" ++ boolString witness.expectPendingTurn
    ++ "}"

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

private def queuedSteeringActionJson : QueuedSteering.Action → String
  | .enqueue => jsonString "enqueue"
  | .claimWithoutBegin => jsonString "claimWithoutBegin"
  | .claimAndBegin => jsonString "claimAndBegin"
  | .latchInterrupt => jsonString "latchInterrupt"
  | .terminate .interruptBeforeClaim => jsonString "interruptBeforeClaim"
  | .terminate .admissionReject => jsonString "admissionReject"
  | .terminate .failBeforeStream => jsonString "failBeforeStream"
  | .terminate .dedupLose => jsonString "dedupLose"
  | .terminate .expire => jsonString "expire"
  | .terminate .interruptClaimed => jsonString "interruptClaimed"
  | .terminate .interruptProcessing => jsonString "interruptProcessing"
  | .terminate .fail => jsonString "fail"
  | .terminate .finish => jsonString "finish"
  | .terminate .bindWorkspace => jsonString "bindWorkspace"
  | .terminate .claim => jsonString "claim"
  | .terminate .beginInference => jsonString "beginInference"
  | .terminate .continueProcessing => jsonString "continueProcessing"
  | .terminate (.parkBackend true) => jsonString "parkBackendClosed"
  | .terminate (.parkBackend false) => jsonString "parkBackendUnclosed"
  | .terminate (.resumeBackend true) => jsonString "resumeBackendReady"
  | .terminate (.resumeBackend false) => jsonString "resumeBackendNotReady"
  | .terminate .beginResumed => jsonString "beginResumed"
  | .terminate .interruptResuming => jsonString "interruptResuming"
  | .terminate .expireResuming => jsonString "expireResuming"
  | .terminate .interruptBackendWait => jsonString "interruptBackendWait"
  | .terminate .expireBackendWait => jsonString "expireBackendWait"
  | .publish => jsonString "publish"
  | .prepareFails => jsonString "prepareFails"
  | .capture => jsonString "capture"
  | .send => jsonString "send"

def queuedSteeringTraceJson (witness : QueuedSteering.TraceObservation) : String :=
  let script := witness.caseScript
  let candidate := script.candidate.map fun prepared =>
    "{\"closing\":" ++ canonicalSegmentJson prepared.closing ++
      ",\"message\":" ++ canonicalMessageJson prepared.message ++ "}"
  let key := script.capture.key
  "{\"name\":" ++ jsonString witness.name ++
    ",\"actions\":" ++ jsonArray (script.actions.map queuedSteeringActionJson) ++
    ",\"requestId\":" ++ toString witness.requestId ++
    ",\"requestDocId\":" ++ toString witness.requestDocId ++
    ",\"contentToken\":" ++ toString witness.contentToken ++
    ",\"entry\":{\"requestId\":" ++ toString script.entry.requestId ++
    ",\"createdAt\":" ++ toString script.entry.createdAt ++
    ",\"source\":" ++ jsonString script.entry.source.toDefraDB ++
    ",\"policy\":" ++ jsonString script.entry.policy.toDefraDB ++
    ",\"queueKey\":" ++ optionalNatJson script.entry.queueKey ++
    ",\"queuedAfter\":" ++ optionalNatJson script.entry.queuedAfter ++ "}" ++
    ",\"interruptAt\":" ++ optionalNatJson script.interruptAt ++
    ",\"preparedCandidate\":" ++ candidate.getD "null" ++
    ",\"capture\":{\"nodeDid\":" ++ toString key.nodeDid ++
    ",\"sessionId\":" ++ toString key.sessionId ++
    ",\"requestDocId\":" ++ toString key.requestId ++
    ",\"turnIndex\":" ++ toString key.turnIndex ++
    ",\"attempt\":" ++ toString key.attempt ++
    ",\"bodyToken\":" ++ toString script.capture.request.value ++
    ",\"priorBodyToken\":" ++ optionalNatJson (script.capture.priorBinding.map (·.value)) ++ "}" ++
    ",\"lifecycleState\":" ++ jsonString witness.lifecycleState ++
    ",\"acceptedInput\":" ++ boolString witness.acceptedInput ++
    ",\"queueActive\":" ++ optionalNatJson witness.queueActive ++
    ",\"admissionVisible\":" ++ boolString witness.admissionVisible ++
    ",\"canonicalAuthoredCount\":" ++ toString witness.canonicalAuthoredCount ++
    ",\"providerSendPermitted\":" ++ boolString witness.providerSendPermitted ++ "}"

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

structure CompactionRowWire where
  role : String
  kind : String
  text : Option String
  callIds : List String
  callId : Option String
  deriving Lean.ToJson

def CompactionRowWire.ofModel (row : Transcript.MessageRow) : CompactionRowWire :=
  match row.kind with
  | .ordinary =>
      { role := row.role.toDefraDB, kind := "ordinary", text := some s!"ordinary-{row.messageId}"
        callIds := [], callId := none }
  | .assistantToolCalls calls =>
      { role := row.role.toDefraDB, kind := "assistant_tool_calls", text := none
        callIds := (calls.sort (· ≤ ·)).map Compaction.fixtureNativeCallKey, callId := none }
  | .toolResult call _ =>
      { role := row.role.toDefraDB, kind := "tool_result", text := none, callIds := []
        callId := some (Compaction.fixtureNativeCallKey call) }

def compactionCallSymbol (key : String) : Nat :=
  if key == "2" then Compaction.fixtureSecondCallSymbol else Compaction.fixtureCallSymbol

def CompactionRowWire.toModel (index : Nat) (row : CompactionRowWire) : Transcript.MessageRow :=
  { messageId := index, sessionId := 0, sequence := index
    role := (Transcript.MessageRole.fromDefraDB? row.role).getD .user
    kind := match row.kind with
      | "assistant_tool_calls" => .assistantToolCalls (row.callIds.map compactionCallSymbol).toFinset
      | "tool_result" => .toolResult (compactionCallSymbol (row.callId.getD "")) ⟨0, 0, 37⟩
      | _ => .ordinary }

def compactionRowsToModel (rows : List CompactionRowWire) : List Transcript.MessageRow :=
  ((List.range rows.length).zip rows).map fun (index, row) => row.toModel index

example : Compaction.compactionReducerCases.all fun c =>
    decide (compactionRowsToModel (c.inputRows.map CompactionRowWire.ofModel) = c.inputRows) := by
  native_decide

example : Compaction.compactionReducerCases.all (·.legal) := by native_decide

deriving instance Lean.ToJson for Compaction.Reducer

structure CompactionReducerInput where
  name : String
  legal : Bool
  reducer : Compaction.Reducer
  inputRows : List CompactionRowWire
  splitIndex : Nat
  publicationReady : Bool
  deriving Lean.ToJson

def compactionReducerOfName : String → Compaction.Reducer
  | "strip" => .strip
  | "summarize" => .summarize
  | "provider_view" => .providerView
  | _ => .identity

def compactionReducerInputs : List CompactionReducerInput :=
  Compaction.compactionReducerCases.map fun c =>
    { name := c.name, legal := c.legal, reducer := compactionReducerOfName c.reducer
      inputRows := c.inputRows.map CompactionRowWire.ofModel, splitIndex := c.splitIndex
      publicationReady := c.publicationReady }

example : Compaction.compactionReducerCases.all fun c =>
    (compactionReducerOfName c.reducer).name == c.reducer := by
  native_decide

def compactionModelCase (i : CompactionReducerInput) : Compaction.CompactionReducerCase :=
  Compaction.reducerCase i.name "" i.reducer 0 i.splitIndex i.publicationReady
    (some (compactionRowsToModel i.inputRows))

def compactionSummarizes (i : CompactionReducerInput) : Bool :=
  match i.reducer with
  | .summarize => true
  | _ => false

def compactionIdentity (i : CompactionReducerInput) : Bool :=
  match i.reducer with
  | .identity => true
  | _ => false

structure CompactionReducerGateOutput where
  projectedPrefixReady : Bool
  preMessageCount : Nat
  turnBoundary : Bool
  providerFixpoint : Bool
  safeToReduce : Bool
  safeBoundary : Nat
  deriving Lean.ToJson

@[conformance_family]
def compactionReducerGate : Family CompactionReducerInput CompactionReducerGateOutput where
  inputs := compactionReducerInputs
  model := fun i =>
    let c := compactionModelCase i
    { projectedPrefixReady := c.projectedPrefixReady, preMessageCount := c.preMessageCount
      turnBoundary := c.turnBoundary, providerFixpoint := c.providerFixpoint
      safeToReduce := c.safeToReduce, safeBoundary := c.safeBoundary }

structure CompactionReducerApplyOutput where
  postMessageCount : Nat
  retainedCount : Nat
  preservesPairs : Bool
  preservesOrder : Bool
  reducerIsIdentity : Bool
  reducerIsIdempotent : Bool
  deriving Lean.ToJson

def compactionReducerApplyOutput (i : CompactionReducerInput) : CompactionReducerApplyOutput :=
  let c := compactionModelCase i
  { postMessageCount := c.postMessageCount, retainedCount := c.retainedCount
    preservesPairs := c.preservesPairs, preservesOrder := c.preservesOrder
    reducerIsIdentity := c.reducerIsIdentity, reducerIsIdempotent := c.reducerIsIdempotent }

@[conformance_family]
def compactionReducerApply : Family CompactionReducerInput CompactionReducerApplyOutput where
  inputs := compactionReducerInputs.filter fun i => !compactionSummarizes i
  model := compactionReducerApplyOutput

/-- model-production-gap: production summarizes by a token budget
(`split_for_summary`); no production reducer applies the model's fixed split
index, so the message counts, pair and order preservation, identity and
idempotence of a summarize step have no native observation. -/
@[conformance_family]
def compactionReducerApplyUnreached :
    Family CompactionReducerInput CompactionReducerApplyOutput where
  inputs := compactionReducerInputs.filter compactionSummarizes
  model := compactionReducerApplyOutput

@[conformance_family]
def compactionReducerGateOpen : Family CompactionReducerInput (Option Bool) where
  inputs := compactionReducerInputs.filter fun i => !compactionIdentity i
  model := fun i => (compactionModelCase i).gateOpen

/-- model-production-gap: the identity reducer exists only in the model; no
production compaction path evaluates its validity gate. -/
@[conformance_family]
def compactionReducerIdentityGateUnreached : Family CompactionReducerInput (Option Bool) where
  inputs := compactionReducerInputs.filter compactionIdentity
  model := fun i => (compactionModelCase i).gateOpen

structure CompactionCursorInput where
  compacted : Nat
  deriving Lean.ToJson

@[conformance_family]
def compactionCursor : Family CompactionCursorInput (Option Nat) where
  inputs := Compaction.compactionCursorCases.map fun c => { compacted := c.compacted }
  model := fun i => Compaction.findCursor Compaction.cursorFixture i.compacted

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
