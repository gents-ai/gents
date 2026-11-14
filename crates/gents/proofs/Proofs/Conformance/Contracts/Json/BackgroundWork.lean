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

namespace Conformance.Contracts.R6

open Lean Conformance.ContractCases

def grouped (group : String) : List R6BackgroundingCase :=
  r6BackgroundingCases.filter (·.group == group)

structure AdmissionInput where
  name : String
  action : String
  preLiveCount : Nat
  /-- An upper bound: a parent never holds more live background rows. -/
  maxBackgrounded : Nat
  deriving ToJson

structure AdmissionOutput where
  legal : Bool
  receiptStatus : Option String
  errorCode : Option String
  deriving ToJson

/-- Every spawn attempt against the parent's live background count, the two
boundary counts and each attempt of a ten-call batch admitted in order. -/
@[conformance_family]
def admission : Family AdmissionInput AdmissionOutput where
  inputs := (grouped "budget" ++ grouped "budget_batch").map fun c =>
    { name := c.name, action := c.action, preLiveCount := c.preLiveCount
      maxBackgrounded := c.maxBackgrounded }
  model := fun i =>
    let c := r6BudgetCase i.name i.preLiveCount
    { legal := c.legal, receiptStatus := if c.legal then some c.terminalState else none
      errorCode := c.errorCode }

structure TransferInput where
  name : String
  action : String
  preLiveCount : Nat
  deriving ToJson

structure TransferOutput where
  legal : Bool
  awaitMode : String
  terminalState : String
  result : Option String
  deriving ToJson

@[conformance_family]
def recoveryTransfer : Family TransferInput TransferOutput where
  inputs := (grouped "budget_recovery").map fun c =>
    { name := c.name, action := c.action, preLiveCount := c.preLiveCount }
  model := fun i =>
    let count := Background.recoverRunningBackground i.preLiveCount
    let handed := ToolExecution.ToolCallContext.step? (r6NativeToolFixture .foreground) .background
    { legal := count == i.preLiveCount + 1
      awaitMode := (handed.map (·.awaitMode.toDefraDB)).getD "foreground"
      terminalState := (handed.map (·.state.toDefraDB)).getD "rejected"
      result := some (toString count) }

structure NativeInput where
  name : String
  action : String
  operation : String
  result : Option String
  reason : Option String
  deriving ToJson

structure NativeOutput where
  legal : Bool
  awaitMode : String
  rowState : String
  cancelCause : Option String
  deriving ToJson

def actionOf : String → ToolExecution.ToolCallContext.Action
  | "background" => .background
  | "complete" => .complete
  | "cancel_during_run" => .cancelDuringRun .userCancelled
  | _ => .foreground

def operationOf : String → ToolExecution.ToolOperation
  | "sessionMessage" => .sessionMessage
  | "pluginJob" => .pluginJob
  | _ => .nativeCommand

def nativePre (i : NativeInput) : ToolExecution.ToolCallContext :=
  { r6NativeToolFixture (if i.action == "background" then .foreground else .background) with
    operation := operationOf i.operation }

def nativeStep (i : NativeInput) : R6BackgroundingCase :=
  r6NativeStepCase i.name i.action (nativePre i) (actionOf i.action) i.result i.reason

@[conformance_family]
def nativeLifecycle : Family NativeInput NativeOutput where
  inputs := (grouped "native_lifecycle").map fun c =>
    { name := c.name, action := c.action, operation := c.operation.getD ""
      result := c.result, reason := c.reason }
  model := fun i =>
    let c := nativeStep i
    { legal := c.legal, awaitMode := c.awaitMode
      rowState := if c.legal then c.terminalState else (nativePre i).state.toDefraDB
      cancelCause := match actionOf i.action with
        | .cancelDuringRun cause => if c.legal then some cause.toDefraDB else none
        | _ => none }

example : (grouped "native_lifecycle").all (fun c =>
    let rebuilt := nativeStep { name := c.name, action := c.action, operation := c.operation.getD ""
                                result := c.result, reason := c.reason }
    rebuilt.legal == c.legal && rebuilt.awaitMode == c.awaitMode &&
      rebuilt.terminalState == c.terminalState && rebuilt.operation == c.operation) := by
  native_decide

structure RestartInput where
  name : String
  awaitMode : String
  parentObservation : String
  processOutcome : String
  sessionKey : String
  deriving ToJson

structure RestartOutput where
  action : String
  terminalState : String
  reason : Option String
  queueSource : Option String
  queueKey : Option String
  deriving ToJson

/-- A restart finds a running background native row whose parent is live and
whose recorded process stopped. -/
@[conformance_family]
def restart : Family RestartInput RestartOutput where
  inputs := (grouped "recovery").map fun c =>
    { name := c.name, awaitMode := c.awaitMode, parentObservation := "live"
      processOutcome := "stopped", sessionKey := "900" }
  model := fun i =>
    let row : Recovery.RestartRow :=
      { awaitMode := (ToolExecution.AwaitMode.fromDefraDB? i.awaitMode).getD .foreground
        kind := .native
        parent := Restart.parentOf i.parentObservation
        deadlineExpired := false
        process := Restart.processOf i.processOutcome }
    let disposition := Recovery.restartDisposition row
    let notification := row.notification
    { action := disposition.causeContract.getD ""
      terminalState := disposition.terminalStateContract.getD "running"
      reason := notification.map (·.notificationReason)
      queueSource := notification.map (·.queueSource)
      queueKey := notification.map (·.queueKeyPrefix ++ i.sessionKey) }

example : (grouped "recovery").map (·.queueKey) = [r6RestartCase.queueKey] ∧
    r6RestartCase.queueKey = some "background_completion:900" := by native_decide

structure QueueSourceInput where
  name : String
  action : String
  queueSource : String
  sessionKey : String
  deriving ToJson

structure QueueSourceOutput where
  legal : Bool
  queueKey : Option String
  deriving ToJson

@[conformance_family]
def queueSource : Family QueueSourceInput QueueSourceOutput where
  inputs := (grouped "queue_source").map fun c =>
    { name := c.name, action := c.action, queueSource := c.queueSource.getD "", sessionKey := "900" }
  model := fun i =>
    let parsed := SessionQueue.QueueSource.fromDefraDB? i.queueSource
    let enqueue := i.action == r6CompletionQueueCase.action
    { legal := if enqueue then parsed.isSome else parsed.isNone
      queueKey := if enqueue then parsed.map (·.toDefraDB ++ ":" ++ i.sessionKey) else none }

structure ContinuationInput where
  name : String
  action : String
  sessionKey : String
  deriving ToJson

structure ContinuationOutput where
  terminalState : String
  waitPrecedesNotification : Bool
  continuationClaimed : Bool
  queueSource : Option String
  queueKey : Option String
  deriving ToJson

@[conformance_family]
def continuation : Family ContinuationInput ContinuationOutput where
  inputs := (grouped "completion_continuation").map fun c =>
    { name := c.name, action := c.action, sessionKey := "900" }
  model := fun i =>
    let wake := BackgroundCompletion.canonicalWake
    let queued := (BackgroundCompletion.appendNotification? BackgroundCompletion.canonicalCompletion
        BackgroundCompletion.canonicalWaitReservedTranscript).bind fun notification =>
      BackgroundCompletion.enqueueWake? notification BackgroundCompletion.canonicalQueue
    { terminalState := BackgroundCompletion.canonicalCompletion.toolState.toDefraDB
      waitPrecedesNotification := BackgroundCompletion.canonicalWaitOrderingAccepted
      continuationClaimed := (queued.bind BackgroundCompletion.claimContinuation?).isSome
      queueSource := some wake.source.toDefraDB
      queueKey := wake.queueKey.map fun _ => wake.source.toDefraDB ++ ":" ++ i.sessionKey }

def wakeOf (requestId retryCount maxRetries depth : Nat) (parent deadline : Option Nat)
    (source : SessionQueue.QueueSource) : BackgroundCompletion.FailedWake :=
  let base := BackgroundCompletion.failedWakeFixture (retryCount := retryCount)
    (maxRetries := maxRetries) (source := source)
  { base with
    requestId := requestId
    ctx := { base.ctx with depth := depth, parentRequestId := parent, deadline := deadline } }

def redriveFixture : BackgroundCompletion.FailedWake :=
  BackgroundCompletion.failedWakeFixture (retryCount := 1)

/-- The failed wake the scenario publishes after the completion, to redrive. -/
structure GoalOwnerInput where
  name : String
  action : String
  goalStatus : Option String
  wakeRequestId : Nat
  wakeRetryCount : Nat
  wakeMaxRetries : Nat
  wakeDepth : Nat
  wakeParentRequestId : Option Nat
  wakeDeadline : Option Nat
  deriving ToJson

structure GoalOwnerOutput where
  legal : Bool
  terminalState : String
  notificationPersisted : Bool
  wakeCreated : Bool
  redriveAllowed : Bool
  successorRetryCount : Option Nat
  successorDepth : Option Nat
  deriving ToJson

def notified := BackgroundCompletion.appendNotification?
  BackgroundCompletion.canonicalCompletion BackgroundCompletion.canonicalWaitReservedTranscript

def wakeQueued := notified.bind fun notification =>
  BackgroundCompletion.enqueueWake? notification BackgroundCompletion.canonicalQueue

/-- A completion notifies its session and wakes it, and a failed wake of that
session redrives, whatever Goal state the session holds. -/
@[conformance_family]
def goalOwnerSideEffects : Family GoalOwnerInput GoalOwnerOutput where
  inputs := (grouped "completion_continuation_owner").map fun c =>
    { name := c.name, action := c.action, goalStatus := c.goalStatus
      wakeRequestId := redriveFixture.requestId, wakeRetryCount := redriveFixture.ctx.retryCount
      wakeMaxRetries := redriveFixture.ctx.maxRetries, wakeDepth := redriveFixture.ctx.depth
      wakeParentRequestId := redriveFixture.ctx.parentRequestId
      wakeDeadline := redriveFixture.ctx.deadline }
  model := fun i =>
    let redrive := BackgroundCompletion.redriveWake? (wakeOf i.wakeRequestId i.wakeRetryCount
      i.wakeMaxRetries i.wakeDepth i.wakeParentRequestId i.wakeDeadline .backgroundCompletion)
    { legal := notified.isSome
      terminalState := BackgroundCompletion.canonicalCompletion.toolState.toDefraDB
      notificationPersisted := notified.isSome
      wakeCreated := wakeQueued.isSome
      redriveAllowed := redrive.isSome
      successorRetryCount := redrive.map (·.retryCount)
      successorDepth := redrive.map (·.depth) }

structure GoalPublicationInput where
  name : String
  action : String
  goalStatus : Option String
  deriving ToJson

structure GoalPublicationOutput where
  notificationPersisted : Bool
  wakeCreated : Bool
  deriving ToJson

@[conformance_family]
def goalOwnerPublication : Family GoalPublicationInput GoalPublicationOutput where
  inputs := (grouped "completion_continuation_owner").map fun c =>
    { name := c.name, action := c.action, goalStatus := c.goalStatus }
  model := fun _ =>
    { notificationPersisted := notified.isSome, wakeCreated := wakeQueued.isSome }

structure RedriveInput where
  name : String
  action : String
  terminalState : String
  queueSource : String
  queueKey : Option String
  retryCount : Nat
  maxRetries : Nat
  preDepth : Nat
  preParentRequestId : Option Nat
  preExecutionDeadline : Option Nat
  redriveSourceRequestId : Nat
  isLatest : Bool
  deriving ToJson

structure RedriveOutput where
  legal : Bool
  postRetryCount : Option Nat
  postDepth : Option Nat
  postParentRequestId : Option Nat
  postExecutionDeadline : Option Nat
  retryDelaySeconds : Option Nat
  deriving ToJson

@[conformance_family]
def redrive : Family RedriveInput RedriveOutput where
  inputs := (grouped "completion_redrive").map fun c =>
    { name := c.name, action := c.action, terminalState := c.terminalState
      queueSource := c.queueSource.getD "", queueKey := c.queueKey
      retryCount := c.retryCount.getD 0, maxRetries := c.maxRetries.getD 0
      preDepth := c.preDepth.getD 0, preParentRequestId := c.preParentRequestId
      preExecutionDeadline := c.preExecutionDeadline
      redriveSourceRequestId := c.redriveSourceRequestId.getD 0
      isLatest := c.isLatest.getD true }
  model := fun i =>
    let source := (SessionQueue.QueueSource.fromDefraDB? i.queueSource).getD .user
    let c := r6FailedWakeRedriveCase i.name (wakeOf i.redriveSourceRequestId i.retryCount
      i.maxRetries i.preDepth i.preParentRequestId i.preExecutionDeadline source) i.isLatest
    { legal := c.legal
      postRetryCount := c.postRetryCount, postDepth := c.postDepth
      postParentRequestId := c.postParentRequestId
      postExecutionDeadline := c.postExecutionDeadline
      retryDelaySeconds := c.retryDelaySeconds }

structure WakeAdmissionInput where
  name : String
  action : String
  queueSource : String
  wakeAgeSeconds : Nat
  otherSource : String
  otherAgeSeconds : Nat
  deriving ToJson

structure WakeAdmissionOutput where
  legal : Bool
  deriving ToJson

def candidateOf (requestId : Nat) (source : String) (age : Nat) :
    BackgroundCompletion.AdmissionCandidate :=
  { requestId, ageSeconds := age
    source := (SessionQueue.QueueSource.fromDefraDB? source).getD .user }

def wakeAdmissionInput (name : String) (wake other : BackgroundCompletion.AdmissionCandidate) :
    WakeAdmissionInput :=
  { name, action := "rank_pending_background_wake", queueSource := wake.source.toDefraDB
    wakeAgeSeconds := wake.ageSeconds, otherSource := other.source.toDefraDB
    otherAgeSeconds := other.ageSeconds }

/-- A pending background-completion wake against an older descendant request
of another session; both ages are seconds before the watcher ranks them. -/
@[conformance_family]
def wakeAdmission : Family WakeAdmissionInput WakeAdmissionOutput where
  inputs :=
    [ wakeAdmissionInput "aged_background_wake_precedes_new_descendant"
        BackgroundCompletion.agedWakeFixture BackgroundCompletion.descendantFixture
    , wakeAdmissionInput "fresh_background_wake_preserves_fifo"
        BackgroundCompletion.freshWakeFixture BackgroundCompletion.descendantFixture ]
  model := fun i =>
    let c := r6WakeAdmissionCase i.name (candidateOf 901 i.queueSource i.wakeAgeSeconds)
      (candidateOf 902 i.otherSource i.otherAgeSeconds)
    { legal := c.legal }

example : wakeAdmission.inputs.map (·.name) = (grouped "completion_admission").map (·.name) := by
  native_decide

structure ScopeWire where
  requestId : String
  sessionId : String
  nodeDid : String
  requesterDid : Option String
  deriving ToJson

def ScopeWire.toModel (s : ScopeWire) : Background.ProcessControl.Scope :=
  processScope s.requestId s.sessionId s.nodeDid s.requesterDid

structure ProcessControlInput where
  name : String
  action : String
  reason : String
  caller : ScopeWire
  owner : ScopeWire
  deriving ToJson

structure ProcessControlOutput where
  legal : Bool
  deriving ToJson

def processControlInput (name action reason : String) (caller owner : ScopeWire) :
    ProcessControlInput :=
  { name, action, reason, caller, owner }

def scopeWire (requestId sessionId nodeDid : String) (requesterDid : Option String) : ScopeWire :=
  { requestId, sessionId, nodeDid, requesterDid }

def processControlInputs : List ProcessControlInput :=
  let same (request : String) := scopeWire request "session-1" "did:agent" (some "did:requester")
  [ processControlInput "list_processes_same_requester_next_turn_authorized"
      "list_processes" "same_requester_next_turn" (same "request-2") (same "request-1")
  , processControlInput "read_process_same_requester_next_turn_authorized"
      "read_process" "same_requester_next_turn" (same "request-2") (same "request-1")
  , processControlInput "wait_process_same_requester_next_turn_authorized"
      "wait_process" "same_requester_next_turn" (same "request-2") (same "request-1")
  , processControlInput "cancel_process_same_requester_next_turn_authorized"
      "cancel_process" "same_requester_next_turn" (same "request-2") (same "request-1")
  , processControlInput "originating_request_without_matching_requester_is_denied"
      "read_process" "originating_request_missing_requester"
      (same "request-1") (scopeWire "request-1" "session-1" "did:agent" none)
  , processControlInput "absent_requester_next_turn_authorized"
      "read_process" "absent_requester_next_turn"
      (scopeWire "request-2" "session-1" "did:agent" none)
      (scopeWire "request-1" "session-1" "did:agent" none)
  , processControlInput "empty_requester_does_not_alias_absent"
      "read_process" "empty_requester_vs_absent"
      (scopeWire "request-2" "session-1" "did:agent" (some ""))
      (scopeWire "request-1" "session-1" "did:agent" none)
  , processControlInput "process_control_cross_session_denied"
      "cancel_process" "cross_session"
      (scopeWire "request-2" "session-2" "did:agent" (some "did:requester")) (same "request-1")
  , processControlInput "process_control_cross_agent_denied"
      "wait_process" "cross_agent"
      (scopeWire "request-2" "session-1" "did:other" (some "did:requester")) (same "request-1")
  , processControlInput "process_control_cross_requester_denied"
      "list_processes" "cross_requester"
      (scopeWire "request-2" "session-1" "did:agent" (some "did:other")) (same "request-1") ]

@[conformance_family]
def processControl : Family ProcessControlInput ProcessControlOutput where
  inputs := processControlInputs
  model := fun i =>
    { legal := (r6ProcessControlCase i.name i.action i.reason i.caller.toModel i.owner.toModel).legal }

example : processControlInputs.map (fun i =>
    let c := r6ProcessControlCase i.name i.action i.reason i.caller.toModel i.owner.toModel
    (c.name, c.action, c.reason, c.legal)) =
    (grouped "process_control_authorization").map (fun c => (c.name, c.action, c.reason, c.legal)) := by
  native_decide

def boundaryOf : String → Background.ProcessControl.WaitBoundary
  | "callerInterrupted" => .callerInterrupted
  | "callerDeadline" => .callerDeadline
  | _ => .waitTimeout

structure WaitInput where
  name : String
  action : String
  boundary : String
  deriving ToJson

structure WaitBoundaryOutput where
  legal : Bool
  terminalState : String
  deriving ToJson

def waitBoundaryInputs : List WaitInput :=
  [ { name := "wait_timeout_preserves_running_process", action := "wait_process"
      boundary := "waitTimeout" }
  , { name := "caller_interrupt_preserves_running_process", action := "wait_process"
      boundary := "callerInterrupted" }
  , { name := "caller_deadline_preserves_running_process", action := "wait_process"
      boundary := "callerDeadline" } ]

/-- A wait on a running background process ends at its own timeout, at the
caller's interruption or at the caller's deadline; the process keeps running. -/
@[conformance_family]
def waitBoundary : Family WaitInput WaitBoundaryOutput where
  inputs := waitBoundaryInputs
  model := fun i =>
    let c := r6WaitBoundaryCase i.name (boundaryOf i.boundary)
    { legal := c.legal, terminalState := c.terminalState }

example : waitBoundaryInputs.map (fun i =>
    let c := r6WaitBoundaryCase i.name (boundaryOf i.boundary)
    (c.name, c.legal, c.terminalState, c.reason)) =
    (grouped "wait_boundary").map (fun c => (c.name, c.legal, c.terminalState, c.reason)) := by
  native_decide

structure WaitDispatchOutput where
  legal : Bool
  awaitMode : String
  terminalState : String
  result : Option String
  deriving ToJson

def dispatchCase (i : WaitInput) : R6BackgroundingCase :=
  match boundaryOf i.boundary with
  | .callerDeadline => r6CallerDeadlineDispatchCase
  | _ => r6CallerInterruptObserverCompletionCase

/-- The wait call itself, foreground on its caller, when the caller's deadline
passes or when an interruption reaches the observing wait. -/
@[conformance_family]
def waitDispatch : Family WaitInput WaitDispatchOutput where
  inputs :=
    [ { name := r6CallerDeadlineDispatchCase.name, action := r6CallerDeadlineDispatchCase.action
        boundary := "callerDeadline" }
    , { name := r6CallerInterruptObserverCompletionCase.name
        action := r6CallerInterruptObserverCompletionCase.action, boundary := "callerInterrupted" } ]
  model := fun i =>
    let c := dispatchCase i
    { legal := c.legal, awaitMode := c.awaitMode, terminalState := c.terminalState
      result := c.result }

structure WaitSettlementInput where
  name : String
  action : String
  deriving ToJson

structure WaitSettlementOutput where
  awaitMode : String
  terminalState : String
  deriving ToJson

/-- An interruption that drops the observing wait settles the wait call
through its terminal parent; the observed process is never cancelled
(`r6_caller_interrupt_dispatch_uses_both_existing_owners`). -/
@[conformance_family]
def waitSettlement : Family WaitSettlementInput WaitSettlementOutput where
  inputs := [{ name := r6CallerInterruptDispatchCase.name, action := r6CallerInterruptDispatchCase.action }]
  model := fun _ =>
    { awaitMode := r6CallerInterruptDispatchCase.awaitMode
      terminalState := r6CallerInterruptDispatchCase.terminalState }

structure AcknowledgementInput where
  name : String
  action : String
  terminalState : String
  sessionKey : String
  deriving ToJson

structure DeliveryOutput where
  attempted : Nat
  acknowledged : Nat
  queueSource : Option String
  queueKey : Option String
  deriving ToJson

def wakeQueue (sessionKey : String) : Option String × Option String :=
  let wake := BackgroundCompletion.canonicalWake
  (some wake.source.toDefraDB, wake.queueKey.map fun _ => wake.source.toDefraDB ++ ":" ++ sessionKey)

/-- A claimed wake holding the session's one bound notification ends in the
terminal state the scenario commits. -/
@[conformance_family]
def acknowledgement : Family AcknowledgementInput DeliveryOutput where
  inputs := (grouped "completion_acknowledgement").map fun c =>
    { name := c.name, action := c.action, terminalState := c.terminalState, sessionKey := "900" }
  model := fun i =>
    let snapshot := BackgroundCompletion.canonicalSnapshotFixture
    let state := (RequestState.fromDefraDB? i.terminalState).getD .failed
    { attempted := snapshot.attemptedBindings.length
      acknowledged := (snapshot.acknowledgedBindings state).length
      queueSource := (wakeQueue i.sessionKey).1
      queueKey := (wakeQueue i.sessionKey).2 }

structure CrashInput where
  name : String
  action : String
  point : String
  sessionKey : String
  deriving ToJson

structure CrashOutput where
  terminalState : String
  attempted : Nat
  acknowledged : Nat
  queueSource : Option String
  queueKey : Option String
  deriving ToJson

def pointOf : String → BackgroundCompletion.DeliveryCrashPoint
  | "beforeClaim" => .beforeClaim
  | "duringInference" => .duringInference
  | _ => .duringAcknowledgement

def crashInput (name point : String) : CrashInput :=
  { name, point, action := deliveryCrashAction (pointOf point), sessionKey := "900" }

/-- The runtime restarts with the session's one bound notification on a wake
that is still pending, that is claimed with its owner still inferring, or that
has committed its completion. -/
@[conformance_family]
def crashBoundary : Family CrashInput CrashOutput where
  inputs :=
    [ crashInput "restart_before_claim_preserves_pending_notification" "beforeClaim"
    , crashInput "live_inference_retains_snapshot_without_ack_or_redrive" "duringInference"
    , crashInput "acknowledgement_projection_restart_is_atomic" "duringAcknowledgement" ]
  model := fun i =>
    let recovered := BackgroundCompletion.recoverDeliveryCrash (pointOf i.point)
    { terminalState := recovered.requestState.toDefraDB
      attempted := recovered.attemptedBindings.length
      acknowledged := recovered.acknowledgedBindings.length
      queueSource := (wakeQueue i.sessionKey).1
      queueKey := (wakeQueue i.sessionKey).2 }

example : (((grouped "wait_boundary" ++ grouped "wait_dispatch_boundary").map
    (fun c => (c.name, c.reason))) ==
    [ ("wait_timeout_preserves_running_process", some "wait_timeout")
    , ("caller_interrupt_preserves_running_process", some "caller_interrupted")
    , ("caller_deadline_preserves_running_process", some "caller_deadline_exceeded")
    , ("caller_deadline_times_out_wait_call_preserves_background_process",
        some "caller_deadline_exceeded")
    , ("caller_interrupt_cancels_wait_call_preserves_background_process",
        some "caller_interrupted")
    , ("caller_interrupt_observer_completes_wait_call_preserves_background_process",
        some "caller_interrupted") ]) = true := by
  native_decide

example : crashBoundary.inputs.map (fun i => (i.name, i.action)) =
    (grouped "completion_failure_boundary").map (fun c => (c.name, c.action)) := by native_decide

example : (((grouped "completion_acknowledgement" ++ grouped "completion_failure_boundary").map
    (fun c => (c.name, c.legal, c.terminalState, c.preLiveCount))) ==
    [ ("completed_wake_acknowledges_exact_claim_snapshot", true, "completed", 1)
    , ("failed_wake_retains_claim_snapshot_unacknowledged", true, "failed", 1)
    , ("restart_before_claim_preserves_pending_notification", true, "pending", 0)
    , ("live_inference_retains_snapshot_without_ack_or_redrive", true, "processing", 1)
    , ("acknowledgement_projection_restart_is_atomic", true, "completed", 1) ]) = true := by
  native_decide

example : r6BackgroundingCases.all (fun c =>
    c.maxBackgrounded == 8 && decide (c.preLiveCount ≤ c.maxBackgrounded)) := by native_decide

example : (r6BackgroundingCases.filterMap (fun c =>
    if c.name ∈ [ "background_tool_budget_count_7_admits_spawn"
                , "background_tool_budget_count_8_rejects_spawn"
                , "tool_kind_bridge_complete_persists_result"
                , "tool_kind_explicit_cancel_projects_explicit_cancel"
                , "tool_kind_background_mode_executes"
                , "background_recovery_running_live_parent_to_cancelled"
                , "generic_scheduled_failure_is_not_background_redrive"
                , "aged_background_wake_precedes_new_descendant"
                , "fresh_background_wake_preserves_fifo"
                , "completed_wake_acknowledges_exact_claim_snapshot"
                , "failed_wake_retains_claim_snapshot_unacknowledged"
                , "restart_before_claim_preserves_pending_notification"
                , "live_inference_retains_snapshot_without_ack_or_redrive"
                , "acknowledgement_projection_restart_is_atomic"
                , "caller_deadline_times_out_wait_call_preserves_background_process" ]
    then some (c.name, c.group, c.action, c.preLiveCount, c.result, c.reason, c.errorCode)
    else none) ==
    [ ("background_tool_budget_count_7_admits_spawn", "budget", "spawn_process", 7, none, none, none)
    , ("background_tool_budget_count_8_rejects_spawn", "budget", "spawn_process", 8, none, none,
        some "background_tool_budget_exceeded")
    , ("tool_kind_background_mode_executes", "native_lifecycle", "background", 1, none, none, none)
    , ("tool_kind_bridge_complete_persists_result", "native_lifecycle", "complete", 1, some "done",
        none, none)
    , ("tool_kind_explicit_cancel_projects_explicit_cancel", "native_lifecycle", "cancel_during_run",
        1, none, some "explicit_cancel", none)
    , ("background_recovery_running_live_parent_to_cancelled", "recovery",
        "TerminalizeBackgroundedAsInterrupted", 1, none, some "interrupted_on_restart", none)
    , ("generic_scheduled_failure_is_not_background_redrive", "completion_redrive",
        "redrive_failed_background_wake", 1, none, some "ineligible", none)
    , ("aged_background_wake_precedes_new_descendant", "completion_admission",
        "rank_pending_background_wake", 1, none, some "aged_priority", none)
    , ("fresh_background_wake_preserves_fifo", "completion_admission",
        "rank_pending_background_wake", 1, none, some "fifo", none)
    , ("completed_wake_acknowledges_exact_claim_snapshot", "completion_acknowledgement",
        "snapshot_notification_bindings", 1, some "attempted=1,acknowledged=1",
        some "completed_ack", none)
    , ("failed_wake_retains_claim_snapshot_unacknowledged", "completion_acknowledgement",
        "snapshot_notification_bindings", 1, some "attempted=1,acknowledged=0",
        some "failed_unacknowledged", none)
    , ("restart_before_claim_preserves_pending_notification", "completion_failure_boundary",
        "restart_before_claim", 0, some "attempted=0,acknowledged=0", some "pending_reclaim", none)
    , ("live_inference_retains_snapshot_without_ack_or_redrive", "completion_failure_boundary",
        "preserve_live_inference", 1, some "attempted=1,acknowledged=0", some "owner_still_live", none)
    , ("acknowledgement_projection_restart_is_atomic", "completion_failure_boundary",
        "project_acknowledgement_after_restart", 1, some "attempted=1,acknowledged=1",
        some "atomic_ack_projection", none)
    , ("caller_deadline_times_out_wait_call_preserves_background_process", "wait_dispatch_boundary",
        "wait_process", 1, some "running", some ("caller_deadline_exceeded" : String),
        (none : Option String)) ]) = true := by
  native_decide

example : ((grouped "completion_redrive").map (fun c =>
    (c.name, c.retryCount, c.maxRetries, c.postRetryCount, c.retryDelaySeconds, c.isLatest,
      c.queueSource)) ==
    [ ("failed_background_wake_with_budget_redrives", some 1, some 3, some 2, some 10, some true,
        some "background_completion")
    , ("failed_background_wake_exhausted_budget_stops", some 3, some 3, none, none, some true,
        some "background_completion")
    , ("generic_scheduled_failure_is_not_background_redrive", some 0, some 3, none, none, some true,
        some "user")
    , ("non_latest_background_wake_does_not_redrive", some 0, some 3, none, none, some false,
        some "background_completion") ]) = true := by
  native_decide

end Conformance.Contracts.R6
