import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.ContractCases
import Proofs.Runtime.Background.ToolOutputCases
import Proofs.Conformance.ContractCases.InterruptDisposition
import Proofs.Conformance.Functions.ToolCalls
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session
import Proofs.ToolCalls.PluginJob.Admission
import Proofs.Conformance.Contracts.Json.NativeExecution

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

structure BackgroundSourceWire where
  kind : String
  auxiliaryKind : Option String
  scope : Option Nat
  turn : Option Nat
  attempt : Option Nat
  call : Option Nat
  key : Option Nat
  deriving Lean.ToJson

private def BackgroundSourceWire.ofModel : CanonicalOutput.Source → BackgroundSourceWire
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
  source : BackgroundSourceWire
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
    coordinate := { request := s.coordinate.request, source := BackgroundSourceWire.ofModel s.coordinate.source }
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
  seed : Lean.Json
  accept : Lean.Json
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
      name := c.name, document := c.document, segments := c.segments.map SegmentWire.ofModel
      seed := NativeExecutionContracts.seedWire (CanonicalOutput.Execution.Examples.world 5)
      accept := NativeExecutionContracts.operationWire .acceptForeground }
  model := fun i =>
    match r4cReadToolOutputCanonicalSourceReconstruction.cases.find? (·.name == i.name) with
    | some c => { state := c.expectedState, payload := c.expectedPayload.map (·.map UInt8.toNat) }
    | none => { state := none, payload := none }

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

deriving instance Lean.ToJson for ToolExecution.AwaitMode
deriving instance Lean.ToJson for Background.Interrupt.Disposition

namespace ToolOutputPaging

structure Input where
  firstOffset : Nat
  retainedLen : Nat
  totalBytes : Nat
  offset : Nat
  maxBytes : Nat
  deriving Lean.ToJson

structure Output where
  start : Nat
  sliceLen : Nat
  nextOffset : Nat
  firstAvailableOffset : Nat
  totalBytesOut : Nat
  hasMore : Bool
  deriving Lean.ToJson

end ToolOutputPaging

@[conformance_family]
def toolOutputPaging : Family ToolOutputPaging.Input ToolOutputPaging.Output where
  inputs := toolOutputPagingCases.map fun c =>
    { firstOffset := c.firstOffset, retainedLen := c.retainedLen, totalBytes := c.totalBytes
      offset := c.offset, maxBytes := c.maxBytes }
  model := fun i =>
    let slice := Background.ToolOutput.readSlice
      { firstOffset := i.firstOffset, retainedLen := i.retainedLen, totalBytes := i.totalBytes }
      i.offset i.maxBytes
    { start := slice.start, sliceLen := slice.sliceLen, nextOffset := slice.nextOffset
      firstAvailableOffset := slice.firstAvailableOffset, totalBytesOut := slice.totalBytes
      hasMore := slice.hasMore }

namespace InterruptDisposition

structure Input where
  state : ToolExecution.ToolCallState
  awaitMode : ToolExecution.AwaitMode
  deriving Lean.ToJson

structure Output where
  disposition : Background.Interrupt.Disposition
  deriving Lean.ToJson

structure Interrupted where
  disposition : Background.Interrupt.Disposition
  postState : ToolExecution.ToolCallState
  postAwaitMode : ToolExecution.AwaitMode
  deriving Lean.ToJson

def every : List Input :=
  ToolExecution.ToolCallState.all.flatMap fun state =>
    ToolExecution.AwaitMode.all.map fun awaitMode => { state, awaitMode }

end InterruptDisposition

@[conformance_family]
def interruptDisposition : Family InterruptDisposition.Input InterruptDisposition.Output where
  inputs := InterruptDisposition.every
  model := fun i =>
    { disposition := Background.Interrupt.disposition (interruptDispositionTool i.state i.awaitMode) }

/-- The in-flight interrupt scenario holds a running lifecycle in the hook's
in-flight table; only running rows can be placed there, so only the running
cases are driven through it. `interruptDisposition` replays every case. -/
@[conformance_family]
def interruptInFlight : Family InterruptDisposition.Input InterruptDisposition.Interrupted where
  inputs := InterruptDisposition.every.filter fun i => i.state == .running
  model := fun i =>
    let tool := interruptDispositionTool i.state i.awaitMode
    let post := Background.Interrupt.interruptTool tool
    { disposition := Background.Interrupt.disposition tool
      postState := post.state
      postAwaitMode := post.awaitMode }

namespace BackgroundBudget

/-- `theoremName` and `scenario` label the proved bound and the native
scenario that drives it. The bound is computed by the model; the scenario
performs that many attempts and then the one it bounds. -/
structure Bound where
  theoremName : String
  scenario : String
  numericBound : Nat
  deriving Lean.ToJson

structure KindField where
  key : String
  value : String
  deriving Lean.ToJson

structure AdmissionObservation where
  kindFields : List KindField
  currentBackgrounded : Nat
  maxBackgrounded : Nat
  deriving Lean.ToJson

def liveAfter (admissions : Nat) : Nat :=
  (List.range admissions).foldl (fun live _ => (Background.admitBackground live).getD live) 0

end BackgroundBudget

@[conformance_family]
def backgroundAdmissionBound : Family BackgroundBudget.Bound BackgroundBudget.AdmissionObservation where
  inputs := [{ theoremName := "Background.admitted_background_count_bounded"
               scenario := "background_tool_admission_respects_max_backgrounded_per_parent"
               numericBound := Background.maxBackgroundedPerParent }]
  model := fun i =>
    let live := BackgroundBudget.liveAfter i.numericBound
    let refusal :=
      if (Background.admitBackground live).isNone then
        PluginJob.Refusal.backgroundBudgetExceeded.code
      else none
    { kindFields :=
        ⟨"await_mode", ToolExecution.AwaitMode.background.toDefraDB⟩ ::
          refusal.toList.map fun code => ⟨"error_code_on_violation", code⟩
      currentBackgrounded := live
      maxBackgrounded := Background.maxBackgroundedPerParent }

@[conformance_family]
def backgroundReceiptContention : Family BackgroundBudget.Bound Bool where
  inputs := [{ theoremName := "Background.admission_cohort_receipt_commits"
               scenario := "spawn_receipt_commits_after_bounded_transaction_conflicts"
               numericBound := Background.receiptContentionAttempts - 1 }]
  model := fun i =>
    Background.receiptCommits (List.replicate i.numericBound false ++ [true])
      Background.receiptContentionAttempts

namespace Restart

def kindName : Recovery.RowKind → String
  | .native => "native"
  | .sessionMessage => "sessionMessage"
  | .pluginJob => "pluginJob"

def kindOf : String → Recovery.RowKind
  | "sessionMessage" => .sessionMessage
  | "pluginJob" => .pluginJob
  | _ => .native

def parentOf : String → Recovery.ParentObservation
  | "interrupted" => .interrupted
  | "cleanlyCompleted" => .cleanlyCompleted
  | "otherTerminal" => .otherTerminal
  | "missing" => .missing
  | _ => .live

def processOf : String → ManagedExec.StopOutcome
  | "alreadyExited" => .alreadyExited
  | "stillRunning" => .stillRunning
  | "notOwned" => .notOwned
  | _ => .stopped

structure Input where
  name : String
  awaitMode : ToolExecution.AwaitMode
  kind : String
  sessionMessage : Bool
  parentObservation : String
  deadlineExpired : Bool
  processOutcome : ManagedExec.StopOutcome
  deriving Lean.ToJson

structure Output where
  disposition : String
  cause : Option String
  terminalState : Option String
  notificationReason : Option String
  queueSource : Option String
  queueKeyPrefix : Option String
  deriving Lean.ToJson

structure Classified where
  cause : Option String
  notificationReason : Option String
  deriving Lean.ToJson

def every : List Input :=
  Recovery.restartDispositionCases.map fun c =>
    { name := c.name
      awaitMode := ((ToolExecution.AwaitMode.fromDefraDB? c.awaitMode).getD .foreground)
      kind := kindName c.kind
      sessionMessage := c.kind == .sessionMessage
      parentObservation := c.parentObservation
      deadlineExpired := c.deadlineExpired
      processOutcome := processOf c.processOutcome }

example : Recovery.restartDispositionCases.all (fun c =>
    (ToolExecution.AwaitMode.fromDefraDB? c.awaitMode).isSome &&
      (parentOf c.parentObservation).toContract == c.parentObservation &&
      (processOf c.processOutcome).toContract == c.processOutcome &&
      kindOf (kindName c.kind) == c.kind) := by native_decide

def row (i : Input) : Recovery.RestartRow :=
  { awaitMode := i.awaitMode, kind := kindOf i.kind, parent := parentOf i.parentObservation
    deadlineExpired := i.deadlineExpired, process := i.processOutcome }

def observe (i : Input) : Output :=
  let disposition := Recovery.restartDisposition (row i)
  let notification := (row i).notification
  { disposition := disposition.toContract
    cause := disposition.causeContract
    terminalState := disposition.terminalStateContract
    notificationReason := notification.map (·.notificationReason)
    queueSource := notification.map (·.queueSource)
    queueKeyPrefix := notification.map (·.queueKeyPrefix) }

end Restart

/-- The orphan classifier decides every native background row whose parent
resolves, including the still-running verdict no test host can construct. -/
@[conformance_family]
def restartClassification : Family Restart.Input Restart.Classified where
  inputs := Restart.every.filter fun i =>
    i.awaitMode == .background && i.kind == "native" && i.parentObservation != "missing"
  model := fun i =>
    let observed := Restart.observe i
    { cause := observed.cause, notificationReason := observed.notificationReason }

/-- Startup recovery of an admitted native row whose parent resolves. A test
host cannot hold a process that survives the stop request, so the
still-running verdict is decided by `restartClassification` only. -/
@[conformance_family]
def restartNative : Family Restart.Input Restart.Output where
  inputs := Restart.every.filter fun i =>
    i.kind == "native" && i.parentObservation != "missing" && i.processOutcome != .stillRunning
  model := Restart.observe

@[conformance_family]
def restartSessionMessage : Family Restart.Input Restart.Output where
  inputs := Restart.every.filter fun i => i.sessionMessage && i.parentObservation != "missing"
  model := Restart.observe

@[conformance_family]
def restartMissingParent : Family Restart.Input Restart.Output where
  inputs := Restart.every.filter fun i => i.parentObservation == "missing"
  model := Restart.observe

@[conformance_family]
def restartPluginJob : Family Restart.Input Restart.Output where
  inputs := Restart.every.filter fun i => i.kind == "pluginJob"
  model := Restart.observe

end Conformance.Contracts
