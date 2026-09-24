import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.ContractCases
import Proofs.Background.ToolOutputCases
import Proofs.DurableLineage

/-!
# Background Work JSON

Serializers and fixed witness rows for R4C/R6 backgrounding and transcript
contracts.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

private def toolOutputBytesJson (bytes : List UInt8) : String :=
  jsonArray (bytes.map (fun byte => toString byte.toNat))

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

def r4cSteerAppendPreservesLineageJson
    (witness : R4cWitnesses.SteerAppendPreservesLineage) : String :=
  "{"
    ++ "\"witness\":"
      ++ jsonString "r4c.steer_subagent.append_preserves_lineage" ++ ","
    ++ "\"caller_request_id\":" ++ jsonString witness.callerRequestId ++ ","
    ++ "\"caller_request_doc_id\":" ++ jsonString witness.callerRequestDocId ++ ","
    ++ "\"child_session_id\":" ++ jsonString witness.childSessionId ++ ","
    ++ "\"queued_request_id\":" ++ jsonString witness.queuedRequestId ++ ","
    ++ "\"caused_by_parent_request_id\":"
      ++ jsonString witness.causedByParentRequestId ++ ","
    ++ "\"caused_by_parent_request_doc_id\":"
      ++ jsonString witness.causedByParentRequestDocId ++ ","
    ++ "\"caused_by_parent_tool_call_id_present\":"
      ++ boolString witness.causedByParentToolCallIdPresent ++ ","
    ++ "\"caused_by_parent_tool_call_doc_id_present\":"
    ++ boolString witness.causedByParentToolCallDocIdPresent ++ ","
    ++ "\"lineage_admissible\":" ++ boolString witness.lineageAdmissible ++ ","
    ++ "\"depth_zero_lineage_admissible\":"
      ++ boolString witness.depthZeroLineageAdmissible ++ ","
    ++ "\"background_completion_depth_zero_admissible\":"
      ++ boolString witness.backgroundCompletionDepthZeroAdmissible ++ ","
    ++ "\"queue_source\":" ++ jsonString witness.queueSource ++ ","
    ++ "\"queue_policy\":" ++ jsonString witness.queuePolicy
    ++ "}"

def r4cUnmaterializedChildVisibleJson
    (witness : R4cWitnesses.UnmaterializedChildVisible) : String :=
  "{"
    ++ "\"witness\":"
      ++ jsonString "r4c.list_subagents.unmaterialized_child_visible" ++ ","
    ++ "\"caller_request_id\":" ++ jsonString witness.callerRequestId ++ ","
    ++ "\"bridge_tool_call_id\":" ++ jsonString witness.bridgeToolCallId ++ ","
    ++ "\"child_request_id\":" ++ jsonString witness.childRequestId ++ ","
    ++ "\"child_materialized\":" ++ boolString witness.childMaterialized ++ ","
    ++ "\"bridge_lifecycle_state\":"
      ++ jsonString witness.bridgeLifecycleState ++ ","
    ++ "\"listed_status\":" ++ jsonString witness.listedStatus ++ ","
    ++ "\"listed_under_all_filter\":"
      ++ boolString witness.listedUnderAllFilter ++ ","
    ++ "\"listed_under_running_filter\":"
      ++ boolString witness.listedUnderRunningFilter ++ ","
    ++ "\"read_lifecycle_state\":" ++ jsonString witness.readLifecycleState ++ ","
    ++ "\"read_terminal\":" ++ boolString witness.readTerminal ++ ","
    ++ "\"wait_retryable\":" ++ boolString witness.waitRetryable
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

-- Open and closed reads share the canonical immutable tool source. The
-- executable projection cases separately pin missing/conflict rejection and
-- committed-extent stability under a late suffix.
def r4cReadToolOutputCanonicalSourceReconstruction :
    R4cWitnesses.ReadToolOutputCanonicalSourceReconstruction :=
  let mkCase (name : String) (world : CanonicalOutput.Execution.World)
      (document : Nat) : R4cWitnesses.ToolOutputProjectionCase :=
    let projected := Subagent.ToolOutput.project world document
    { name := name, document := document, segments := world.segments
    , expectedState := projected.map fun projection =>
        match projection.state with | .open => "open" | .closed => "closed"
    , expectedPayload := projected.map (fun projection => projection.bytes) }
  let lateSuffix : CanonicalOutput.Segment :=
    { id := 703, coordinate := ⟨10, .tool 600⟩, writer := .tool 600
    , flush := some ⟨1, [⟨0, 1, none⟩], [33]⟩
    , close := none, createdAt := 5 }
  let cases := match Subagent.ToolOutput.Cases.running,
      Subagent.ToolOutput.Cases.closed with
    | some openWorld, some closedWorld =>
      let conflictWorld := { openWorld with segments := openWorld.segments ++
        [{ Subagent.ToolOutput.Cases.output with id := 702, flush := some ⟨0,
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

-- #593 fixed witness: the bridge exists and is `running`, the child row is
-- absent, and every parent-facing surface stays observable — the list entry
-- projects `awaiting_child_materialization` (visible under both the `all`
-- and default `running` filters), the read explains the same projection
-- without faking a terminal outcome, and the wait payload is retryable.
def r4cUnmaterializedChildVisible :
    R4cWitnesses.UnmaterializedChildVisible :=
  { callerRequestId := "r4c-w7-caller"
  , bridgeToolCallId := "r4c-w7-bridge-call"
  , childRequestId := "r4c-w7-child"
  , childMaterialized := false
  , bridgeLifecycleState := "running"
  , listedStatus := "awaiting_child_materialization"
  , listedUnderAllFilter := true
  , listedUnderRunningFilter := true
  , readLifecycleState := "awaiting_child_materialization"
  , readTerminal := false
  , waitRetryable := true
  }

def r4cSteerAppendPreservesLineage :
    R4cWitnesses.SteerAppendPreservesLineage :=
  let steering := DurableLineage.steeringContinuation 1
  { callerRequestId := "r4c-w5-caller"
  , callerRequestDocId := "bae-r4c-w5-caller"
  , childSessionId := "r4c-w5-child-session"
  , queuedRequestId := "r4c-w5-queued"
  , causedByParentRequestId := "r4c-w5-caller"
  , causedByParentRequestDocId := "bae-r4c-w5-caller"
  , causedByParentToolCallIdPresent := steering.hasParentToolCallId
  , causedByParentToolCallDocIdPresent := steering.hasParentToolCallDocId
  , lineageAdmissible := DurableLineage.admissible steering
  , depthZeroLineageAdmissible :=
      DurableLineage.admissible (DurableLineage.steeringContinuation 0)
  , backgroundCompletionDepthZeroAdmissible :=
      DurableLineage.admissible
        (DurableLineage.backgroundCompletionContinuation 0)
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
  , r4cReadToolOutputCanonicalSourceReconstructionJson
      r4cReadToolOutputCanonicalSourceReconstruction
  , r4cSteerAppendPreservesLineageJson r4cSteerAppendPreservesLineage
  , r4cSteerInterruptComposesJson r4cSteerInterruptComposes
  , r4cUnmaterializedChildVisibleJson r4cUnmaterializedChildVisible
  ]

def bridgeStepCaseJson (witness : BridgeStepCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"event\":" ++ jsonString witness.event ++ ","
    ++ "\"child_state\":" ++ jsonString witness.childState ++ ","
    ++ "\"parent_state\":" ++ jsonString witness.parentState ++ ","
    ++ "\"cancel_policy\":" ++ jsonString witness.cancelPolicy ++ ","
    ++ "\"bridge_committed\":" ++ boolString witness.bridgeCommitted ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"post_tool_state\":" ++ jsonOptionalString witness.postToolState ++ ","
    ++ "\"post_child_interrupt_set\":"
      ++ boolString witness.postChildInterruptSet ++ ","
    ++ "\"theorem\":" ++ jsonString witness.theoremName
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
    ++ "\"cancel_policy\":" ++ jsonString witness.cancelPolicy ++ ","
    ++ "\"child_request_id\":" ++ jsonOptionalString witness.childRequestId ++ ","
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

def r5CrossPrincipalCaseJson (witness : R5CrossPrincipalCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"route\":" ++ jsonString witness.route ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"parent_principal\":" ++ jsonString witness.parentPrincipal ++ ","
    ++ "\"child_principal\":" ++ jsonString witness.childPrincipal ++ ","
    ++ "\"parent_request_id\":" ++ jsonString witness.parentRequestId ++ ","
    ++ "\"parent_tool_call_id\":" ++ jsonString witness.parentToolCallId ++ ","
    ++ "\"child_request_id\":" ++ jsonString witness.childRequestId ++ ","
    ++ "\"target_behavior_id\":" ++ jsonString witness.targetBehaviorId ++ ","
    ++ "\"await_mode\":" ++ jsonString witness.awaitMode ++ ","
    ++ "\"cancel_policy\":" ++ jsonString witness.cancelPolicy ++ ","
    ++ "\"parent_trigger_persisted\":"
      ++ boolString witness.parentTriggerPersisted ++ ","
    ++ "\"child_materialized\":" ++ boolString witness.childMaterialized ++ ","
    ++ "\"child_owned_by_target_principal\":"
      ++ boolString witness.childOwnedByTargetPrincipal ++ ","
    ++ "\"caused_by_parent_request_id_matches\":"
      ++ boolString witness.causedByParentRequestIdMatches ++ ","
    ++ "\"caused_by_parent_tool_call_id_matches\":"
      ++ boolString witness.causedByParentToolCallIdMatches ++ ","
    ++ "\"caused_by_trigger_kind\":"
      ++ jsonString witness.causedByTriggerKind ++ ","
    ++ "\"cross_principal_routing_fired\":"
      ++ boolString witness.crossPrincipalRoutingFired ++ ","
    ++ "\"same_principal_fallback\":"
      ++ boolString witness.samePrincipalFallback ++ ","
    ++ "\"unclaimed_deadline_set\":" ++ boolString witness.unclaimedDeadlineSet
    ++ "}"

def cancelPropagationCaseJson (witness : CancelPropagationCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"route\":" ++ jsonString witness.route ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"parent_principal\":" ++ jsonString witness.parentPrincipal ++ ","
    ++ "\"child_principal\":" ++ jsonString witness.childPrincipal ++ ","
    ++ "\"parent_request_id\":" ++ jsonString witness.parentRequestId ++ ","
    ++ "\"parent_tool_call_id\":" ++ jsonString witness.parentToolCallId ++ ","
    ++ "\"child_request_id\":" ++ jsonString witness.childRequestId ++ ","
    ++ "\"bridge_collection\":" ++ jsonString witness.bridgeCollection ++ ","
    ++ "\"child_request_collection\":"
      ++ jsonString witness.childRequestCollection ++ ","
    ++ "\"cancel_intent_written_on_bridge\":"
      ++ boolString witness.cancelIntentWrittenOnBridge ++ ","
    ++ "\"bridge_cancel_replicates_to_host\":"
      ++ boolString witness.bridgeCancelReplicatesToHost ++ ","
    ++ "\"host_interrupts_child\":"
      ++ boolString witness.hostInterruptsChild ++ ","
    ++ "\"child_interrupt_intent_replicates_to_coordinator\":"
      ++ boolString witness.childInterruptIntentReplicatesToCoordinator ++ ","
    ++ "\"cancel_ack_returns_to_coordinator\":"
      ++ boolString witness.cancelAckReturnsToCoordinator ++ ","
    ++ "\"no_third_party_rows\":" ++ boolString witness.noThirdPartyRows
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

def subagentDelegationGraphCaseJson
    (witness : SubagentDelegationGraphCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"theorem_name\":" ++ jsonString witness.theoremName ++ ","
    ++ "\"property\":" ++ jsonString witness.property ++ ","
    ++ "\"witness_kind\":" ++ jsonString witness.witnessKind ++ ","
    ++ "\"max_depth\":" ++ toString witness.maxDepth ++ ","
    ++ "\"path_length\":" ++ toString witness.pathLength ++ ","
    ++ "\"parent_depth\":" ++ toString witness.parentDepth ++ ","
    ++ "\"terminal_depth\":" ++ toString witness.terminalDepth ++ ","
    ++ "\"cascade_path\":" ++ boolString witness.cascadePath ++ ","
    ++ "\"acyclic\":" ++ boolString witness.acyclic ++ ","
    ++ "\"bounded\":" ++ boolString witness.bounded ++ ","
    ++ "\"cascade_covered\":"
      ++ boolString witness.cascadeCovered ++ ","
    ++ "\"edge_theorem\":" ++ jsonString witness.edgeTheorem ++ ","
    ++ "\"cascade_edge_theorem\":"
      ++ jsonOptionalString witness.cascadeEdgeTheorem
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
