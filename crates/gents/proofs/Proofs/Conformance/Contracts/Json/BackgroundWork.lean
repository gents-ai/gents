import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.ContractCases
import Proofs.Runtime.Background.ToolOutputCases
import Proofs.Conformance.ContractCases.InterruptDisposition
import Proofs.Conformance.Functions.ToolCalls
import Proofs.Conformance.Family
import Proofs.ToolCalls.PluginJob.Admission

namespace Conformance.Contracts

open Conformance.ContractCases

private def toolOutputBytesJson (bytes : List UInt8) : String :=
  jsonArray (bytes.map (fun byte => toString byte.toNat))

def toolOutputProjectionCaseJson
    (value : R4cWitnesses.ToolOutputProjectionCase) : String :=
  "{\"name\":" ++ jsonString value.name ++
    ",\"document\":" ++ toString value.document ++
    ",\"segments\":" ++ jsonArray (value.segments.map canonicalSegmentJson) ++
    ",\"expected_state\":" ++ (match value.expectedState with
      | none => "null" | some state => jsonString state) ++
    ",\"expected_payload\":" ++ (match value.expectedPayload with
      | none => "null" | some payload => toolOutputBytesJson payload) ++ "}"

def r4cReadToolOutputCanonicalSourceReconstructionJson
    (witness : R4cWitnesses.ReadToolOutputCanonicalSourceReconstruction) : String :=
  "{"
    ++ "\"witness\":"
    ++ jsonString "r4c.read_tool_output.canonical_source_reconstruction" ++ ","
    ++ "\"tool_call_id\":" ++ jsonString witness.toolCallId ++ ","
    ++ "\"canonical_source\":" ++ jsonString witness.canonicalSource ++ ","
    ++ "\"cases\":" ++ jsonArray (witness.cases.map toolOutputProjectionCaseJson)
    ++ "}"

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

def r4cBackgroundWorkCasesJson : List String :=
  [ r4cReadToolOutputCanonicalSourceReconstructionJson
      r4cReadToolOutputCanonicalSourceReconstruction
  ]

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

end Conformance.Contracts
