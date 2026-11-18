import Proofs.Session.Transcript.Executable
import Proofs.Clients.SessionSnapshot
import Proofs.Conformance.Family

namespace Conformance.TranscriptCases

open Lean Transcript

structure MessageWire where
  sequence : Nat
  role : String
  deriving ToJson

structure ToolCallWire where
  callId : Nat
  messageSequence : Nat
  state : String
  deriving ToJson

structure SnapshotInput where
  name : String
  messages : List MessageWire
  toolCalls : List ToolCallWire
  deriving ToJson

structure ToolGroupWire where
  messageSequence : Nat
  tools : Nat
  deriving ToJson

structure SnapshotOutput where
  postMessageCount : Nat
  postToolCallCount : Nat
  toolGroups : List ToolGroupWire
  deriving ToJson

def SnapshotInput.ofCase (c : TranscriptCase) : SnapshotInput :=
  { name := c.name
    messages := (SessionSnapshot.messageEntries c.post).map fun m =>
      { sequence := m.sequence, role := m.role.toDefraDB }
    toolCalls := (SessionSnapshot.toolCallEntries c.post).map fun call =>
      { callId := call.callId, messageSequence := call.messageSequence
        state := call.state.toDefraDB } }

def messageEntryOfWire (m : MessageWire) : SessionSnapshot.MessageEntry :=
  { sequence := m.sequence, role := (MessageRole.fromDefraDB? m.role).getD .assistant }

def toolCallEntryOfWire (call : ToolCallWire) : SessionSnapshot.ToolCallEntry :=
  { callId := call.callId, messageSequence := call.messageSequence
    state := (ToolExecution.ToolCallState.fromDefraDB? call.state).getD .pending }

@[conformance_family]
def snapshotProjection : Family SnapshotInput SnapshotOutput where
  inputs := transcriptConformanceCases.map SnapshotInput.ofCase
  model := fun i =>
    let observation := SessionSnapshot.project (i.messages.map messageEntryOfWire)
      (i.toolCalls.map toolCallEntryOfWire)
    { postMessageCount := observation.messageCount
      postToolCallCount := observation.toolCallCount
      toolGroups := observation.toolGroups.map fun group =>
        { messageSequence := group.messageSequence, tools := group.tools } }

structure NativeInput where
  name : String
  actionCallIds : List Nat
  actionLogicalResultIds : List Nat
  actionPayloadHashes : List Nat
  deriving ToJson

structure NativeCompletionOutput where
  assistantSequence : Nat
  postMessageCount : Nat
  messageRoles : List String
  resultIds : List Nat
  toolCallStates : List String
  toolCallResults : List Nat
  maxSequence : Nat
  postInFlightCount : Nat
  deriving ToJson

def NativeInput.ofCase (c : TranscriptCase) : NativeInput :=
  { name := c.name
    actionCallIds := c.actionCallIds
    actionLogicalResultIds := c.actionLogicalResultIds
    actionPayloadHashes := c.actionPayloadHashes }

/-- The native hook fixture starts each case from an empty session and drives user input, one
accepted assistant turn, dispatch and completion of every published call through the canonical
owners. Only these witnesses are such traces from the empty transcript: the publication-only,
cancellation and dispatch-failure witnesses stop before completion, and the duplicate and
distinct-result witnesses start from synthetic result-only states. `snapshotProjection` replays
every witness. -/
def nativeCompletionCases : List TranscriptCase :=
  transcriptConformanceCases.filter fun c =>
    c.name ∈ ["ordering_user_assistant_tool_result", "parallel_results_share_assistant_turn",
      "completed_tool_pair_closed"]

@[conformance_family]
def nativeCompletion : Family NativeInput NativeCompletionOutput where
  inputs := nativeCompletionCases.map NativeInput.ofCase
  model := fun i =>
    let post := ((transcriptConformanceCases.find? fun c => c.name == i.name).map
      TranscriptCase.post).getD ⟨0, 1, [], [], ∅⟩
    let assistant := (post.messages.find? fun row => row.role == .assistant).map MessageRow.sequence
    { assistantSequence := assistant.getD 0
      postMessageCount := post.messageCount
      messageRoles := post.messages.map fun row => row.role.toDefraDB
      resultIds := post.messages.filterMap fun row =>
        row.kind.toolResultKey?.map ToolResultKey.logicalResultId
      toolCallStates := post.toolCalls.map fun call => call.state.toDefraDB
      toolCallResults := post.toolCalls.filterMap fun call =>
        call.resultKey.map ToolResultKey.payloadHash
      maxSequence := (post.messages.map MessageRow.sequence).foldl max 0
      postInFlightCount := post.inFlight.card }

/-- The abandon witness starts from a running call with no persisted header. The native fixture
builds the nearest production state, an accepted header with one dispatched call, and drops the
hook, so it observes the tool-call states the abandonment leaves. -/
def nativeAbandonCases : List TranscriptCase :=
  transcriptConformanceCases.filter fun c => c.name == "drop_abandon_not_strong_drain"

@[conformance_family]
def nativeAbandon : Family NativeInput (List String) where
  inputs := nativeAbandonCases.map NativeInput.ofCase
  model := fun i =>
    let post := ((transcriptConformanceCases.find? fun c => c.name == i.name).map
      TranscriptCase.post).getD ⟨0, 1, [], [], ∅⟩
    post.toolCalls.map fun call => call.state.toDefraDB

example : transcriptConformanceCases.all (·.expectedOrdered) = true := by native_decide

example : (transcriptConformanceCases.filter (·.expectedDuplicateReusedSequence)).all
    (fun c => c.preMessageCount == c.postMessageCount && c.resultSequence == c.postMessageCount) =
      true := by
  native_decide

example : dedupeDuplicateReusesSequenceCase.action = "observe_duplicate_tool_result" ∧
    dedupeDuplicateReusesSequenceCase.expectedDuplicateReusedSequence = true := by
  native_decide

example : distinctResultIdsAppendDistinctRowsCase.action = "append_distinct_tool_result" ∧
    distinctResultIdsAppendDistinctRowsCase.expectedPairClosed = false := by
  native_decide

example : parallelResultsShareAssistantTurnCase.action =
    "publish_once_dispatch_in_order_then_complete_each_parallel_result" := by
  native_decide

end Conformance.TranscriptCases
