import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.ContractCases
import Proofs.Background.ToolOutputCases

/-!
# Background Work JSON

Serializers and fixed witness rows for background-tool and transcript
contracts.
-/

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

-- Open and closed reads share the canonical immutable tool source. The
-- executable projection cases separately pin missing/conflict rejection and
-- committed-extent stability under a late suffix.
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
    ++ "\"redrive_allowed\":" ++ jsonOptionalBool witness.redriveAllowed
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
