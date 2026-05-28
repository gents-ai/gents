import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.StreamingResponse.Executable
import Proofs.Compaction.Executable
import Proofs.Recovery.ContractCases

/-!
# Client and Runtime Support JSON

Serializers for live overlays, streaming responses, compaction, and recovery
sweeps.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def liveOverlayCaseJson (witness : LiveOverlayCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"responseStatus\":" ++ jsonString witness.responseStatus ++ ","
    ++ "\"materialized\":" ++ boolString witness.materialized ++ ","
    ++ "\"precedingToolCalls\":" ++ toString witness.precedingToolCalls ++ ","
    ++ "\"turnTerminal\":" ++ boolString witness.turnTerminal ++ ","
    ++ "\"turnLabel\":" ++ jsonString witness.turnLabel ++ ","
    ++ "\"hasContent\":" ++ boolString witness.hasContent ++ ","
    ++ "\"hasReasoning\":" ++ boolString witness.hasReasoning ++ ","
    ++ "\"expectOverlay\":" ++ boolString witness.expectOverlay
    ++ "}"

def responseTransitionCaseJson
    (witness : StreamingResponse.ResponseTransitionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_status\":" ++ jsonString witness.preStatus ++ ","
    ++ "\"post_status\":" ++ jsonString witness.postStatus ++ ","
    ++ "\"pre_live_tail\":" ++ jsonString witness.preLiveTail ++ ","
    ++ "\"post_live_tail\":" ++ jsonString witness.postLiveTail ++ ","
    ++ "\"pre_token_count\":" ++ toString witness.preTokenCount ++ ","
    ++ "\"post_token_count\":" ++ toString witness.postTokenCount ++ ","
    ++ "\"error_reason\":" ++ jsonOptionalString witness.errorReason ++ ","
    ++ "\"pre_materialized_seq\":"
      ++ jsonOptionalNat witness.preMaterializedSeq ++ ","
    ++ "\"post_materialized_seq\":"
      ++ jsonOptionalNat witness.postMaterializedSeq ++ ","
    ++ "\"expected_request_state\":"
      ++ jsonOptionalString witness.expectedRequestState ++ ","
    ++ "\"expected_request_persistence\":"
      ++ jsonOptionalString witness.expectedRequestPersistence
    ++ "}"

def responseInterruptFlowCaseJson
    (witness : StreamingResponse.ResponseInterruptFlowCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"pre_request_state\":"
      ++ jsonString witness.preRequestState ++ ","
    ++ "\"post_request_state\":"
      ++ jsonString witness.postRequestState ++ ","
    ++ "\"pre_response_status\":"
      ++ jsonString witness.preResponseStatus ++ ","
    ++ "\"post_response_status\":"
      ++ jsonString witness.postResponseStatus ++ ","
    ++ "\"pre_inference_call_state\":"
      ++ jsonString witness.preInferenceCallState ++ ","
    ++ "\"post_inference_call_state\":"
      ++ jsonString witness.postInferenceCallState ++ ","
    ++ "\"response_error_reason\":"
      ++ jsonString witness.responseErrorReason ++ ","
    ++ "\"interrupted_at_required\":"
      ++ boolString witness.interruptedAtRequired ++ ","
    ++ "\"completed_at_required\":"
      ++ boolString witness.completedAtRequired ++ ","
    ++ "\"live_tail_cleared\":"
      ++ boolString witness.liveTailCleared ++ ","
    ++ "\"partial_turn_materialized\":"
      ++ boolString witness.partialTurnMaterialized ++ ","
    ++ "\"request_terminal\":"
      ++ boolString witness.requestTerminal ++ ","
    ++ "\"response_terminal\":"
      ++ boolString witness.responseTerminal ++ ","
    ++ "\"inference_call_terminal\":"
      ++ boolString witness.inferenceCallTerminal
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
    ++ "\"gate_open\":" ++ boolString witness.gateOpen ++ ","
    ++ "\"safe_to_reduce\":" ++ boolString witness.safeToReduce ++ ","
    ++ "\"reducer_is_identity\":"
      ++ boolString witness.reducerIsIdentity
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
    ++ "\"deadline_audit_ref\":"
    ++ jsonString witness.deadlineAuditRef
    ++ "}"

end Conformance.Contracts
