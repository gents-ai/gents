import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases

/-!
# Background Work JSON

Serializers and fixed witness rows for R4C/R6 backgrounding and transcript
contracts.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def r4cListSubagentsLineageRejectsJson
    (witness : R4cWitnesses.ListSubagentsLineageRejects) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString "r4c.list_subagents.lineage_rejects" ++ ","
    ++ "\"caller_request_id\":" ++ jsonString witness.callerRequestId ++ ","
    ++ "\"sibling_request_id\":" ++ jsonString witness.siblingRequestId ++ ","
    ++ "\"sibling_child_id\":" ++ jsonString witness.siblingChildId ++ ","
    ++ "\"caller_sees_sibling_child\":"
      ++ boolString witness.callerSeesSiblingChild
    ++ "}"

def r4cReadTranscriptCursorAdvancesJson
    (witness : R4cWitnesses.ReadTranscriptCursorAdvances) : String :=
  "{"
    ++ "\"witness\":"
      ++ jsonString "r4c.read_subagent_transcript.cursor_advances" ++ ","
    ++ "\"child_session_id\":" ++ jsonString witness.childSessionId ++ ","
    ++ "\"first_since_sequence\":" ++ toString witness.firstSinceSequence ++ ","
    ++ "\"first_through_sequence\":"
      ++ toString witness.firstThroughSequence ++ ","
    ++ "\"first_next_sequence\":" ++ toString witness.firstNextSequence ++ ","
    ++ "\"second_since_sequence\":"
      ++ toString witness.secondSinceSequence ++ ","
    ++ "\"second_through_sequence\":"
      ++ toString witness.secondThroughSequence ++ ","
    ++ "\"no_gap\":" ++ boolString witness.noGap ++ ","
    ++ "\"no_overlap\":" ++ boolString witness.noOverlap
    ++ "}"

def r4cReadTranscriptHidesBridgeRowsJson
    (witness : R4cWitnesses.ReadTranscriptHidesBridgeRows) : String :=
  "{"
    ++ "\"witness\":"
      ++ jsonString "r4c.read_subagent_transcript.hides_bridge_rows" ++ ","
    ++ "\"child_session_id\":" ++ jsonString witness.childSessionId ++ ","
    ++ "\"bridge_call_id\":" ++ jsonString witness.bridgeCallId ++ ","
    ++ "\"rendered_transcript\":" ++ jsonString witness.renderedTranscript
    ++ "}"

def r4cReadToolOutputDispatchesByStateJson
    (witness : R4cWitnesses.ReadToolOutputDispatchesByState) : String :=
  "{"
    ++ "\"witness\":"
      ++ jsonString "r4c.read_tool_output.dispatch_by_state" ++ ","
    ++ "\"tool_call_id\":" ++ jsonString witness.toolCallId ++ ","
    ++ "\"running_source\":" ++ jsonString witness.runningSource ++ ","
    ++ "\"terminal_source\":" ++ jsonString witness.terminalSource ++ ","
    ++ "\"running_payload\":" ++ jsonString witness.runningPayload ++ ","
    ++ "\"stale_running_payload\":"
      ++ jsonString witness.staleRunningPayload ++ ","
    ++ "\"terminal_payload\":" ++ jsonString witness.terminalPayload
    ++ "}"

def r4cSteerAppendPreservesLineageJson
    (witness : R4cWitnesses.SteerAppendPreservesLineage) : String :=
  "{"
    ++ "\"witness\":"
      ++ jsonString "r4c.steer_subagent.append_preserves_lineage" ++ ","
    ++ "\"caller_request_id\":" ++ jsonString witness.callerRequestId ++ ","
    ++ "\"child_session_id\":" ++ jsonString witness.childSessionId ++ ","
    ++ "\"queued_request_id\":" ++ jsonString witness.queuedRequestId ++ ","
    ++ "\"caused_by_parent_request_id\":"
      ++ jsonString witness.causedByParentRequestId ++ ","
    ++ "\"queue_source\":" ++ jsonString witness.queueSource ++ ","
    ++ "\"queue_policy\":" ++ jsonString witness.queuePolicy
    ++ "}"

def r4cSteerInterruptComposesJson
    (witness : R4cWitnesses.SteerInterruptComposes) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString "r4c.steer_subagent.interrupt_composes" ++ ","
    ++ "\"caller_request_id\":" ++ jsonString witness.callerRequestId ++ ","
    ++ "\"child_session_id\":" ++ jsonString witness.childSessionId ++ ","
    ++ "\"interrupted_active_request_id\":"
      ++ jsonString witness.interruptedActiveRequestId ++ ","
    ++ "\"drained_wake_up_request_ids\":"
      ++ jsonStringArray witness.drainedWakeUpRequestIds ++ ","
    ++ "\"drained_wake_up_queue_key\":"
      ++ jsonString witness.drainedWakeUpQueueKey ++ ","
    ++ "\"queued_request_id\":" ++ jsonString witness.queuedRequestId ++ ","
    ++ "\"queue_interrupted_request_id\":"
      ++ jsonString witness.queueInterruptedRequestId
    ++ "}"

def r4cListSubagentsLineageRejects :
    R4cWitnesses.ListSubagentsLineageRejects :=
  { callerRequestId := "r4c-w1-caller"
  , siblingRequestId := "r4c-w1-sibling"
  , siblingChildId := "r4c-w1-sibling-child"
  , callerSeesSiblingChild := false
  }

def r4cReadTranscriptCursorAdvances :
    R4cWitnesses.ReadTranscriptCursorAdvances :=
  { childSessionId := "r4c-w2-session"
  , firstSinceSequence := 0
  , firstThroughSequence := 5
  , firstNextSequence := 6
  , secondSinceSequence := 6
  , secondThroughSequence := 10
  , noGap := true
  , noOverlap := true
  }

def r4cReadTranscriptHidesBridgeRows :
    R4cWitnesses.ReadTranscriptHidesBridgeRows :=
  { childSessionId := "r4c-w3-session"
  , bridgeCallId := "r4c-w3-bridge-call"
  , renderedTranscript := "[assistant seq=2]\nplain assistant message\n"
  }

def r4cReadToolOutputDispatchesByState :
    R4cWitnesses.ReadToolOutputDispatchesByState :=
  { toolCallId := "r4c-w4-tool-call"
  , runningSource := "ring_buffer"
  , terminalSource := "persisted_tool_completion"
  , runningPayload := "ring-buffer-live-tail"
  , staleRunningPayload := "stale-ring-buffer-tail"
  , terminalPayload := "persisted-completion-stdout"
  }

def r4cSteerAppendPreservesLineage :
    R4cWitnesses.SteerAppendPreservesLineage :=
  { callerRequestId := "r4c-w5-caller"
  , childSessionId := "r4c-w5-child-session"
  , queuedRequestId := "r4c-w5-queued"
  , causedByParentRequestId := "r4c-w5-caller"
  , queueSource := "steering"
  , queuePolicy := "append"
  }

def r4cSteerInterruptComposes :
    R4cWitnesses.SteerInterruptComposes :=
  { callerRequestId := "r4c-w6-caller"
  , childSessionId := "r4c-w6-child-session"
  , interruptedActiveRequestId := "r4c-w6-interrupted"
  , drainedWakeUpRequestIds := ["r4c-w6-wake-1", "r4c-w6-wake-2"]
  , drainedWakeUpQueueKey := "background_completion:r4c-w6-child-session"
  , queuedRequestId := "r4c-w6-queued"
  , queueInterruptedRequestId := "r4c-w6-interrupted"
  }

def r4cBackgroundWorkCasesJson : List String :=
  [ r4cListSubagentsLineageRejectsJson r4cListSubagentsLineageRejects
  , r4cReadTranscriptCursorAdvancesJson r4cReadTranscriptCursorAdvances
  , r4cReadTranscriptHidesBridgeRowsJson r4cReadTranscriptHidesBridgeRows
  , r4cReadToolOutputDispatchesByStateJson r4cReadToolOutputDispatchesByState
  , r4cSteerAppendPreservesLineageJson r4cSteerAppendPreservesLineage
  , r4cSteerInterruptComposesJson r4cSteerInterruptComposes
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
    ++ "\"cancel_policy\":" ++ jsonString witness.cancelPolicy ++ ","
    ++ "\"child_request_id\":" ++ jsonOptionalString witness.childRequestId ++ ","
    ++ "\"terminal_state\":" ++ jsonString witness.terminalState ++ ","
    ++ "\"result\":" ++ jsonOptionalString witness.result ++ ","
    ++ "\"reason\":" ++ jsonOptionalString witness.reason ++ ","
    ++ "\"error_code\":" ++ jsonOptionalString witness.errorCode ++ ","
    ++ "\"queue_source\":" ++ jsonOptionalString witness.queueSource ++ ","
    ++ "\"queue_key\":" ++ jsonOptionalString witness.queueKey
    ++ "}"

def transcriptCaseJson (witness : TranscriptCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
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
