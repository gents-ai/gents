import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.StreamingResponse.Executable
import Proofs.Compaction.Executable
import Proofs.Recovery.ContractCases
import Proofs.QueuedSteering

namespace Conformance.Contracts

open Conformance.ContractCases

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
    ++ "\"unrelatedUserTurns\":" ++ toString witness.unrelatedUserTurns ++ ","
    ++ "\"expectPendingTurn\":" ++ boolString witness.expectPendingTurn
    ++ "}"

def queuedSteeringGuardJson (witness : QueuedSteering.GuardObservation) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"admitted\":" ++ boolString witness.admitted
    ++ "}"

private def tagged (kind fields : String) : String :=
  "{\"kind\":" ++ jsonString kind ++ fields ++ "}"

private def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

private def byteArrayJson (bytes : List UInt8) : String :=
  jsonArray (bytes.map (fun byte => toString byte.toNat))

private def sourceJson : CanonicalOutput.Source → String
  | .provider scope turn attempt => tagged "provider"
      (",\"scope\":" ++ toString scope ++ ",\"turn\":" ++ toString turn ++
       ",\"attempt\":" ++ toString attempt)
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
  | .opaque => "opaque" | .arguments => "arguments" | .toolOutput => "tool_output"
  | .media => "media"

private def mediaKindJson : CanonicalOutput.MediaKind → String
  | .image => "image" | .audio => "audio" | .video => "video" | .document => "document"

private def optionalStringJson : Option String → String
  | none => "null" | some value => jsonString value

private def payloadRefJson (ref : CanonicalOutput.PayloadRef) : String :=
  "{\"close_id\":" ++ toString ref.closeId ++ ",\"stream\":" ++ toString ref.stream ++ "}"

private def presentationJson : CanonicalOutput.Presentation → String
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
    ",\"blocks\":" ++ jsonArray (native.blocks.map (messageBlockJson byteArrayJson)) ++ "}"

def canonicalMessageJson (message : CanonicalOutput.MessageEnvelope) : String :=
  let h := message.header
  "{\"header\":{\"id\":" ++ toString h.id ++ ",\"session\":" ++ toString h.session ++
    ",\"request\":" ++ optionalNatJson h.request ++ ",\"origin\":" ++ optionalNatJson h.origin ++
    ",\"refs\":" ++ jsonArray (h.refs.map payloadRefJson) ++ ",\"outcome\":" ++ outcomeJson h.outcome ++
    ",\"role\":" ++ jsonString (roleJson h.role) ++ ",\"publication\":" ++ publicationJson h.publication ++
    "},\"key\":" ++ jsonString message.key ++ ",\"sequence\":" ++ toString message.sequence ++
    ",\"native_id\":" ++ optionalStringJson message.nativeId ++ ",\"blocks\":" ++
    jsonArray (message.blocks.map (messageBlockJson payloadSpecJson)) ++
    ",\"created_at\":" ++ toString message.createdAt ++ "}"

private def queuedSteeringActionJson : QueuedSteering.Action → String
  | .enqueue => jsonString "enqueue"
  | .claimWithoutBegin => jsonString "claimWithoutBegin"
  | .claimAndBegin => jsonString "claimAndBegin"
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
    ",\"capture\":{\"agentDid\":" ++ toString key.agentDid ++
    ",\"sessionId\":" ++ toString key.sessionId ++
    ",\"requestDocId\":" ++ toString key.requestId ++
    ",\"turnIndex\":" ++ toString key.turnIndex ++
    ",\"attempt\":" ++ toString key.attempt ++
    ",\"bodyToken\":" ++ toString script.capture.request.value ++
    ",\"priorBodyToken\":" ++ optionalNatJson (script.capture.priorBinding.map (·.value)) ++ "}" ++
    ",\"lifecycleState\":" ++ jsonString witness.lifecycleState ++
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
    ++ "\"expected\":" ++ outputViewJson witness.expected
    ++ "}"

def compactionReducerCaseJson (witness : Compaction.CompactionReducerCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"reducer\":" ++ jsonString witness.reducer ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
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
    ++ "\"unclaimed_expired\":" ++ jsonOptionalBool witness.unclaimedExpired ++ ","
    ++ "\"parent_live\":" ++ jsonOptionalBool witness.parentLive ++ ","
    ++ "\"parent_interrupted\":" ++ jsonOptionalBool witness.parentInterrupted ++ ","
    ++ "\"parent_terminal\":" ++ jsonOptionalBool witness.parentTerminal ++ ","
    ++ "\"execution_registered\":"
      ++ jsonOptionalBool witness.executionRegistered ++ ","
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
    ++ "\"cancel_policy\":" ++ jsonString witness.cancelPolicy ++ ","
    ++ "\"child_linked\":" ++ boolString witness.childLinked ++ ","
    ++ "\"parent_observation\":"
      ++ jsonString witness.parentObservation ++ ","
    ++ "\"deadline_expired\":" ++ boolString witness.deadlineExpired ++ ","
    ++ "\"unclaimed_expired\":" ++ boolString witness.unclaimedExpired ++ ","
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
