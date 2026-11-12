import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.ContractCases
import Proofs.Runtime.Background.ToolOutputCases
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

def r4cReadToolOutputCanonicalSourceReconstruction :
    R4cWitnesses.ReadToolOutputCanonicalSourceReconstruction :=
  let mkCase (name : String) (world : CanonicalOutput.Execution.World)
      (document : Nat) : R4cWitnesses.ToolOutputProjectionCase :=
    let projected := Background.ToolOutput.project world document
    { name := name, document := document, segments := world.segments
    , expectedState := projected.map fun projection =>
        match projection.state with | .open => "open" | .closed => "closed"
    , expectedPayload := projected.map (fun projection => projection.bytes) }
  let lateSuffix : CanonicalOutput.Segment :=
    { id := 703, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
    , flush := some ⟨1, [⟨0, 1, none⟩], [33]⟩
    , close := none, createdAt := 5 }
  let cases := match Background.ToolOutput.Cases.running,
      Background.ToolOutput.Cases.closed with
    | some openWorld, some closedWorld =>
      let conflictWorld := { openWorld with segments := openWorld.segments ++
        [{ Background.ToolOutput.Cases.output with id := 702, flush := some ⟨0,
          [⟨0, 4, some { block := 0, part := 0, kind := .toolOutput }⟩],
          [66, 65, 68, 33]⟩ }] }
      let lateWorld := { closedWorld with segments := closedWorld.segments ++
        [lateSuffix] }
      [mkCase "open" openWorld 600, mkCase "closed" closedWorld 600,
        mkCase "missing" openWorld 601, mkCase "conflict" conflictWorld 600,
        mkCase "late_suffix" lateWorld 600]
    | _, _ => []
  { toolCallId := "r4c-w4-tool-call"
  , canonicalSource := "canonical_tool_segments"
  , cases := cases
  }

structure SourceWire where
  kind : String
  auxiliaryKind : Option String
  scope : Option Nat
  turn : Option Nat
  attempt : Option Nat
  call : Option Nat
  key : Option Nat
  deriving Lean.ToJson

private def SourceWire.ofModel : CanonicalOutput.Source → SourceWire
  | .provider scope turn attempt => ⟨"provider", none, some scope, some turn, some attempt, none, none⟩
  | .auxiliary kind scope turn attempt =>
      ⟨"auxiliary", some (match kind with
        | .compaction => "compaction"
        | .compactionFallback => "compaction_fallback"
        | .title => "title"
        | .accessReview => "access_review"), some scope, some turn, some attempt, none, none⟩
  | .toolEffectArguments call => ⟨"tool_effect_arguments", none, none, none, none, some call, none⟩
  | .tool call => ⟨"tool", none, none, none, none, some call, none⟩
  | .authored key => ⟨"authored", none, none, none, none, none, some key⟩

structure CoordinateWire where
  request : Nat
  source : SourceWire
  deriving Lean.ToJson

structure WriterWire where
  kind : String
  generation : Option Nat
  call : Option Nat
  deriving Lean.ToJson

private def WriterWire.ofModel : CanonicalOutput.Writer → WriterWire
  | .request generation => ⟨"request", some generation, none⟩
  | .tool call => ⟨"tool", none, some call⟩

structure ToolWire where
  id : String
  callId : Option String
  name : String
  deriving Lean.ToJson

structure DeclarationWire where
  block : Nat
  part : Nat
  kind : String
  tool : Option ToolWire
  mediaKind : Option String
  deriving Lean.ToJson

private def payloadKindName : CanonicalOutput.PayloadKind → String
  | .text => "text" | .reasoning => "reasoning" | .summary => "summary"
  | .signature => "signature" | .reasoningStop => "reasoning_stop"
  | .encrypted => "encrypted" | .redacted => "redacted"
  | .arguments => "arguments" | .toolOutput => "tool_output"
  | .media => "media"

private def mediaKindName : CanonicalOutput.MediaKind → String
  | .image => "image" | .audio => "audio" | .video => "video" | .document => "document"

private def DeclarationWire.ofModel (d : CanonicalOutput.Declaration) : DeclarationWire :=
  { block := d.block, part := d.part, kind := payloadKindName d.kind
    tool := d.tool.map fun t => { id := t.id, callId := t.callId, name := t.name }
    mediaKind := d.mediaKind.map mediaKindName }

structure RunWire where
  stream : Nat
  bytes : Nat
  declaration : Option DeclarationWire
  deriving Lean.ToJson

structure FlushWire where
  ordinal : Nat
  runs : List RunWire
  payload : List Nat
  deriving Lean.ToJson

structure ClosureWire where
  kind : String
  outcome : Option String
  segments : Option Nat
  streamBytes : Option (List Nat)
  deriving Lean.ToJson

private def ClosureWire.ofModel : CanonicalOutput.Closure → ClosureWire
  | .retracted => ⟨"retracted", none, none, none⟩
  | .closed outcome segments streamBytes =>
      ⟨"closed", some (match outcome with | .complete => "complete" | .partial => "partial"),
        some segments, some streamBytes⟩

structure SegmentWire where
  id : Nat
  coordinate : CoordinateWire
  writer : WriterWire
  flush : Option FlushWire
  close : Option ClosureWire
  createdAt : Nat
  deriving Lean.ToJson

private def SegmentWire.ofModel (s : CanonicalOutput.Segment) : SegmentWire :=
  { id := s.id
    coordinate := { request := s.coordinate.request, source := SourceWire.ofModel s.coordinate.source }
    writer := WriterWire.ofModel s.writer
    flush := s.flush.map fun f =>
      { ordinal := f.ordinal
        runs := f.runs.map fun r =>
          { stream := r.stream, bytes := r.bytes, declaration := r.declaration.map DeclarationWire.ofModel }
        payload := f.payload.map UInt8.toNat }
    close := s.close.map ClosureWire.ofModel
    createdAt := s.createdAt }

structure ToolOutputInput where
  witness : String
  toolCallId : String
  canonicalSource : String
  name : String
  document : Nat
  segments : List SegmentWire
  deriving Lean.ToJson

structure ToolOutputObservation where
  state : Option String
  payload : Option (List Nat)
  deriving Lean.ToJson

@[conformance_family]
def toolOutputCanonicalSource : Family ToolOutputInput ToolOutputObservation where
  inputs := r4cReadToolOutputCanonicalSourceReconstruction.cases.map fun c =>
    { witness := "r4c.read_tool_output.canonical_source_reconstruction"
      toolCallId := r4cReadToolOutputCanonicalSourceReconstruction.toolCallId
      canonicalSource := r4cReadToolOutputCanonicalSourceReconstruction.canonicalSource
      name := c.name, document := c.document, segments := c.segments.map SegmentWire.ofModel }
  model := fun i =>
    match r4cReadToolOutputCanonicalSourceReconstruction.cases.find? (·.name == i.name) with
    | some c => { state := c.expectedState, payload := c.expectedPayload.map (·.map UInt8.toNat) }
    | none => { state := none, payload := none }

def interruptDispositionCaseJson (witness : InterruptDispositionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"state\":" ++ jsonString witness.state ++ ","
    ++ "\"await_mode\":" ++ jsonString witness.awaitMode ++ ","
    ++ "\"disposition\":" ++ jsonString witness.disposition ++ ","
    ++ "\"post_state\":" ++ jsonString witness.postState ++ ","
    ++ "\"post_await_mode\":" ++ jsonString witness.postAwaitMode
    ++ "}"

def toolOutputPagingCaseJson (witness : ToolOutputPagingCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"first_offset\":" ++ toString witness.firstOffset ++ ","
    ++ "\"retained_len\":" ++ toString witness.retainedLen ++ ","
    ++ "\"total_bytes\":" ++ toString witness.totalBytes ++ ","
    ++ "\"offset\":" ++ toString witness.offset ++ ","
    ++ "\"max_bytes\":" ++ toString witness.maxBytes ++ ","
    ++ "\"start\":" ++ toString witness.start ++ ","
    ++ "\"slice_len\":" ++ toString witness.sliceLen ++ ","
    ++ "\"next_offset\":" ++ toString witness.nextOffset ++ ","
    ++ "\"first_available_offset\":"
      ++ toString witness.firstAvailableOffset ++ ","
    ++ "\"total_bytes_out\":" ++ toString witness.totalBytesOut ++ ","
    ++ "\"has_more\":" ++ boolString witness.hasMore ++ ","
    ++ "\"theorem\":" ++ jsonString witness.theoremName
    ++ "}"

def r6BackgroundingCaseJson (witness : R6BackgroundingCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_live_count\":" ++ toString witness.preLiveCount ++ ","
    ++ "\"max_backgrounded\":" ++ toString witness.maxBackgrounded ++ ","
    ++ "\"await_mode\":" ++ jsonString witness.awaitMode ++ ","
    ++ "\"terminal_state\":" ++ jsonString witness.terminalState ++ ","
    ++ "\"result\":" ++ jsonOptionalString witness.result ++ ","
    ++ "\"reason\":" ++ jsonOptionalString witness.reason ++ ","
    ++ "\"error_code\":" ++ jsonOptionalString witness.errorCode ++ ","
    ++ "\"queue_source\":" ++ jsonOptionalString witness.queueSource ++ ","
    ++ "\"queue_key\":" ++ jsonOptionalString witness.queueKey ++ ","
    ++ "\"retry_count\":" ++ jsonOptionalNat witness.retryCount ++ ","
    ++ "\"max_retries\":" ++ jsonOptionalNat witness.maxRetries ++ ","
    ++ "\"post_retry_count\":" ++ jsonOptionalNat witness.postRetryCount ++ ","
    ++ "\"redrive_source_request_id\":" ++ jsonOptionalNat witness.redriveSourceRequestId ++ ","
    ++ "\"pre_depth\":" ++ jsonOptionalNat witness.preDepth ++ ","
    ++ "\"post_depth\":" ++ jsonOptionalNat witness.postDepth ++ ","
    ++ "\"pre_parent_request_id\":" ++ jsonOptionalNat witness.preParentRequestId ++ ","
    ++ "\"post_parent_request_id\":" ++ jsonOptionalNat witness.postParentRequestId ++ ","
    ++ "\"pre_execution_deadline\":" ++ jsonOptionalNat witness.preExecutionDeadline ++ ","
    ++ "\"post_execution_deadline\":" ++ jsonOptionalNat witness.postExecutionDeadline ++ ","
    ++ "\"retry_delay_seconds\":" ++ jsonOptionalNat witness.retryDelaySeconds ++ ","
    ++ "\"is_latest\":" ++ jsonOptionalBool witness.isLatest ++ ","
    ++ "\"goal_status\":" ++ jsonOptionalString witness.goalStatus ++ ","
    ++ "\"notification_persisted\":" ++ jsonOptionalBool witness.notificationPersisted ++ ","
    ++ "\"wake_created\":" ++ jsonOptionalBool witness.wakeCreated ++ ","
    ++ "\"redrive_allowed\":" ++ jsonOptionalBool witness.redriveAllowed ++ ","
    ++ "\"operation\":" ++ jsonOptionalString witness.operation
    ++ "}"

def backgroundTheoremWitnessJson (witness : BackgroundTheoremWitness) : String :=
  "{"
    ++ "\"theorem_name\":" ++ jsonString witness.theoremName ++ ","
    ++ "\"witness_kind\":" ++ jsonString witness.witnessKind ++ ","
    ++ "\"scenario\":" ++ jsonString witness.scenario ++ ","
    ++ "\"numeric_bound\":" ++ toString witness.numericBound ++ ","
    ++ "\"kind_fields\":"
      ++ jsonArray (witness.kindFields.map (fun (key, value) =>
            "{"
              ++ "\"key\":" ++ jsonString key ++ ","
              ++ "\"value\":" ++ jsonString value
              ++ "}"))
    ++ "}"

def transcriptCaseJson (witness : TranscriptCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"action_call_ids\":" ++ jsonArray (witness.actionCallIds.map toString) ++ ","
    ++ "\"action_logical_result_ids\":"
      ++ jsonArray (witness.actionLogicalResultIds.map toString) ++ ","
    ++ "\"action_payload_hashes\":"
      ++ jsonArray (witness.actionPayloadHashes.map toString) ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_message_count\":" ++ toString witness.preMessageCount ++ ","
    ++ "\"post_message_count\":" ++ toString witness.postMessageCount ++ ","
    ++ "\"pre_tool_call_count\":" ++ toString witness.preToolCallCount ++ ","
    ++ "\"post_tool_call_count\":" ++ toString witness.postToolCallCount ++ ","
    ++ "\"pre_in_flight_count\":" ++ toString witness.preInFlightCount ++ ","
    ++ "\"post_in_flight_count\":" ++ toString witness.postInFlightCount ++ ","
    ++ "\"assistant_sequence\":" ++ toString witness.assistantSequence ++ ","
    ++ "\"result_sequence\":" ++ toString witness.resultSequence ++ ","
    ++ "\"logical_result_id\":" ++ toString witness.logicalResultId ++ ","
    ++ "\"payload_hash\":" ++ toString witness.payloadHash ++ ","
    ++ "\"expected_pair_closed\":" ++ boolString witness.expectedPairClosed ++ ","
    ++ "\"expected_ordered\":" ++ boolString witness.expectedOrdered ++ ","
    ++ "\"expected_duplicate_reused_sequence\":"
      ++ boolString witness.expectedDuplicateReusedSequence ++ ","
    ++ "\"expected_strong_drain\":" ++ boolString witness.expectedStrongDrain
    ++ "}"

end Conformance.Contracts
