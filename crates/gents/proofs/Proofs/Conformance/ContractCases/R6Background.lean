import Proofs.Background.State
import Proofs.Background.ToolOutput
import Proofs.Background.CompletionContinuation
import Proofs.Background.ProcessControl
import Proofs.Conformance.ContractCases.Types
import Proofs.Recovery.Sweeps.BackgroundRestart
import Proofs.Session.State
import Proofs.ToolExecution.Executable

namespace Conformance.ContractCases

def r6Case
    (name group action : String)
    (legal : Bool)
    (preLiveCount : Nat)
    (terminalState : String)
    (result reason errorCode queueSource queueKey : Option String := none) :
    R6BackgroundingCase :=
  { name := name
  , group := group
  , action := action
  , legal := legal
  , preLiveCount := preLiveCount
  , maxBackgrounded := Subagent.maxBackgroundedPerParent
  , awaitMode := "background"
  , cancelPolicy := "cascade"
  , childRequestId := none
  , terminalState := terminalState
  , result := result
  , reason := reason
  , errorCode := errorCode
  , queueSource := queueSource
  , queueKey := queueKey
  }

/-- Concrete childless native-tool row used to execute the R6 lifecycle.
The production `new_background_tool` constructor starts pending/background;
`start_running` supplies the running/committed shape modeled here. -/
def r6NativeToolFixture
    (awaitMode : Subagent.AwaitMode := .background) :
    ToolExecution.ToolCallContext :=
  { callId := 77
  , requestId := 900
  , state := .running
  , operation := .nativeCommand
  , deadline := 100
  , startedAt := some 1
  , currentTime := 10
  , failureClass := none
  , persistence := .committed
  , approval := none
  , awaitMode := awaitMode
  , cancelPolicy := .cascade
  , childRequestId := none
  }

/-- Execute one native-tool action and project its actual post-state into the
R6 JSON row. No caller supplies `legal`, `terminalState`, mode, policy, or
child-link values. -/
def r6NativeStepCase
    (name actionName : String)
    (pre : ToolExecution.ToolCallContext)
    (action : ToolExecution.ToolCallContext.Action)
    (result reason : Option String := none) : R6BackgroundingCase :=
  let base :=
    r6Case name "native_lifecycle" actionName false 1 "rejected"
      result reason
  match ToolExecution.ToolCallContext.step? pre action with
  | none => base
  | some post =>
      { base with
          legal := true
        , awaitMode := post.awaitMode.toDefraDB
        , cancelPolicy := post.cancelPolicy.toDefraDB
        , childRequestId := post.childRequestId.map toString
        , terminalState := post.state.toDefraDB
      }

/-- Admission is the executable numeric guard enforced by Rust before
creating another live background row. -/
def r6BudgetCase (name : String) (preLiveCount : Nat) :
    R6BackgroundingCase :=
  let legal := decide (preLiveCount < Subagent.maxBackgroundedPerParent)
  r6Case name "budget" "spawn_process" legal preLiveCount
    (if legal then "running" else "rejected")
    none none
    (if legal then none else some "background_tool_budget_exceeded")

/-- The R6 startup row is computed by the same total restart classifier that
drives `ToolCallLifecycle::recover_all` conformance. -/
def r6RestartCase : R6BackgroundingCase :=
  let row : Recovery.RestartRow :=
    { awaitMode := .background
    , cancelPolicy := .cascade
    , childLinked := false
    , parent := .live
    , deadlineExpired := false
    , unclaimedExpired := false
    }
  let disposition := Recovery.restartDisposition row
  let notification := disposition.notification
  { name := "background_recovery_running_live_parent_to_cancelled"
  , group := "recovery"
  , action := disposition.causeContract.getD ""
  , legal := true
  , preLiveCount := 1
  , maxBackgrounded := Subagent.maxBackgroundedPerParent
  , awaitMode := row.awaitMode.toDefraDB
  , cancelPolicy := row.cancelPolicy.toDefraDB
  , childRequestId := if row.childLinked then some "linked" else none
  , terminalState := disposition.terminalStateContract.getD "running"
  , result := none
  , reason := notification.map (·.notificationReason)
  , errorCode := none
  , queueSource := notification.map (·.queueSource)
  , queueKey := notification.map (fun obligation =>
      obligation.queueKeyPrefix ++ "900")
  }

def r6CompletionQueueCase : R6BackgroundingCase :=
  let source := SessionQueue.QueueSource.backgroundCompletion
  r6Case
    "background_completion_source_writes_canonical_key"
    "queue_source"
    "enqueue_background_completion"
    true
    1
    "completed"
    (some "done")
    none
    none
    (some source.toDefraDB)
    (some (source.toDefraDB ++ ":900"))

/-- The terminal → notification → coalesced wake → claimed continuation row is
computed by the composed executable acceptance model. -/
def r6CompletionContinuationCase : R6BackgroundingCase :=
  let accepted := BackgroundCompletion.canonicalContinuationAccepted
  let completion := BackgroundCompletion.canonicalCompletion
  let wake := BackgroundCompletion.canonicalWake
  r6Case
    "terminal_completion_message_precedes_claimed_continuation"
    "completion_continuation"
    "terminalize_append_notification_enqueue_claim"
    accepted
    1
    completion.toolState.toDefraDB
    (if accepted then some "assistant_wait_precedes_notification" else none)
    (if accepted then some "continuation_claimed" else none)
    none
    (some wake.source.toDefraDB)
    (wake.queueKey.map fun key => wake.source.toDefraDB ++ ":" ++ toString key)

def r6LegacyQueueAliasCase : R6BackgroundingCase :=
  let legacy := "subagent_completion"
  let parsed := SessionQueue.QueueSource.fromDefraDB? legacy
  r6Case
    "legacy_subagent_completion_source_aliases_canonical_key"
    "queue_source"
    "parse_legacy_subagent_completion"
    parsed.isSome
    1
    "completed"
    (some "done")
    none
    none
    (some legacy)
    (parsed.map fun source => source.toDefraDB ++ ":900")

def processScope
    (requestId sessionId agentDid : String)
    (requesterDid : Option String) : Subagent.ProcessControl.Scope :=
  { requestId, sessionId, agentDid, requesterDid }

def r6ProcessControlCase
    (name action scenario : String)
    (caller owner : Subagent.ProcessControl.Scope) : R6BackgroundingCase :=
  r6Case name "process_control_authorization" action
    (Subagent.ProcessControl.authorized caller owner)
    1 "running" none (some scenario)

def childTerminalContract : Subagent.ChildTerminal -> String
  | .running => "running"
  | .completed => "completed"
  | .failed => "failed"
  | .dead => "dead"
  | .interrupted => "interrupted"
  | .superseded => "superseded"

def r6WaitBoundaryCase
    (name : String) (boundary : Subagent.ProcessControl.WaitBoundary) :
    R6BackgroundingCase :=
  let observation :=
    Subagent.ProcessControl.observeBoundary Subagent.ChildTerminal.running boundary
  r6Case name "wait_boundary" "wait_process"
    (!observation.cancellationRequested)
    1 (childTerminalContract observation.processState)
    none (some observation.reason)

def r6BackgroundingCases : List R6BackgroundingCase :=
  [ r6BudgetCase
      "background_tool_budget_count_7_admits_spawn"
      7
  , r6BudgetCase
      "background_tool_budget_count_8_rejects_spawn"
      8
  , r6NativeStepCase
      "tool_kind_background_mode_executes"
      "background"
      (r6NativeToolFixture .foreground)
      .background
  , r6NativeStepCase
      "tool_kind_bridge_complete_persists_result"
      "bridge_complete"
      r6NativeToolFixture
      .complete
      (some "done")
  , r6NativeStepCase
      "tool_kind_explicit_cancel_projects_explicit_cancel"
      "bridge_failure"
      r6NativeToolFixture
      (.cancelDuringRun .interrupted)
      none
      (some "explicit_cancel")
  , r6RestartCase
  , r6CompletionQueueCase
  , r6CompletionContinuationCase
  , r6LegacyQueueAliasCase
  , r6ProcessControlCase
      "list_processes_same_requester_next_turn_authorized"
      "list_processes" "same_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "read_process_same_requester_next_turn_authorized"
      "read_process" "same_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "wait_process_same_requester_next_turn_authorized"
      "wait_process" "same_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "cancel_process_same_requester_next_turn_authorized"
      "cancel_process" "same_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "originating_request_authorizes_legacy_row_without_requester"
      "read_process" "originating_request_legacy_row"
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" none)
  , r6ProcessControlCase
      "absent_requester_next_turn_authorized"
      "read_process" "absent_requester_next_turn"
      (processScope "request-2" "session-1" "did:agent" none)
      (processScope "request-1" "session-1" "did:agent" none)
  , r6ProcessControlCase
      "empty_requester_does_not_alias_absent"
      "read_process" "empty_requester_vs_absent"
      (processScope "request-2" "session-1" "did:agent" (some ""))
      (processScope "request-1" "session-1" "did:agent" none)
  , r6ProcessControlCase
      "process_control_cross_session_denied"
      "cancel_process" "cross_session"
      (processScope "request-2" "session-2" "did:agent" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "process_control_cross_agent_denied"
      "wait_process" "cross_agent"
      (processScope "request-2" "session-1" "did:other" (some "did:requester"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6ProcessControlCase
      "process_control_cross_requester_denied"
      "list_processes" "cross_requester"
      (processScope "request-2" "session-1" "did:agent" (some "did:other"))
      (processScope "request-1" "session-1" "did:agent" (some "did:requester"))
  , r6WaitBoundaryCase
      "wait_timeout_preserves_running_process"
      .waitTimeout
  , r6WaitBoundaryCase
      "caller_interrupt_preserves_running_process"
      .callerInterrupted
  , r6WaitBoundaryCase
      "caller_deadline_preserves_running_process"
      .callerDeadline
  ]

/-- Pin the concrete projections while keeping their construction executable:
changing the mode guard, native completion/cancel state, budget, restart
classifier, or queue vocabulary changes this tuple and fails `lake build`. -/
theorem r6BackgroundingCases_pinned :
    r6BackgroundingCases.map
        (fun witness =>
          (witness.name, witness.legal, witness.awaitMode,
            witness.childRequestId, witness.terminalState,
            witness.queueSource, witness.queueKey)) =
      [ ("background_tool_budget_count_7_admits_spawn", true, "background",
          none, "running", none, none)
      , ("background_tool_budget_count_8_rejects_spawn", false, "background",
          none, "rejected", none, none)
      , ("tool_kind_background_mode_executes", true, "background",
          none, "running", none, none)
      , ("tool_kind_bridge_complete_persists_result", true, "background",
          none, "completed", none, none)
      , ("tool_kind_explicit_cancel_projects_explicit_cancel",
          true, "background", none, "cancelled", none, none)
      , ("background_recovery_running_live_parent_to_cancelled", true,
          "background", none, "cancelled", some "background_completion",
          some "background_completion:900")
      , ("background_completion_source_writes_canonical_key", true,
          "background", none, "completed", some "background_completion",
          some "background_completion:900")
      , ("terminal_completion_message_precedes_claimed_continuation", true,
          "background", none, "completed", some "background_completion",
          some "background_completion:900")
      , ("legacy_subagent_completion_source_aliases_canonical_key", true,
          "background", none, "completed", some "subagent_completion",
          some "background_completion:900")
      , ("list_processes_same_requester_next_turn_authorized", true,
          "background", none, "running", none, none)
      , ("read_process_same_requester_next_turn_authorized", true,
          "background", none, "running", none, none)
      , ("wait_process_same_requester_next_turn_authorized", true,
          "background", none, "running", none, none)
      , ("cancel_process_same_requester_next_turn_authorized", true,
          "background", none, "running", none, none)
      , ("originating_request_authorizes_legacy_row_without_requester", true,
          "background", none, "running", none, none)
      , ("absent_requester_next_turn_authorized", true,
          "background", none, "running", none, none)
      , ("empty_requester_does_not_alias_absent", false,
          "background", none, "running", none, none)
      , ("process_control_cross_session_denied", false,
          "background", none, "running", none, none)
      , ("process_control_cross_agent_denied", false,
          "background", none, "running", none, none)
      , ("process_control_cross_requester_denied", false,
          "background", none, "running", none, none)
      , ("wait_timeout_preserves_running_process", true,
          "background", none, "running", none, none)
      , ("caller_interrupt_preserves_running_process", true,
          "background", none, "running", none, none)
      , ("caller_deadline_preserves_running_process", true,
          "background", none, "running", none, none)
      ] := by
  rfl

/-! ## Tool output paging witnesses (#937)

Outputs are computed from `Subagent.ToolOutput.readSlice`; the pinned tuple
theorem below fails at Lean build time if the slice model drifts, and the
Rust `background_tools` unit test fails if `read_retained_output_slice`
drifts from the emitted rows. -/

def toolOutputPagingCase
    (name : String)
    (firstOffset retainedLen totalBytes offset maxBytes : Nat)
    (theoremName : String) : ToolOutputPagingCase :=
  let window : Subagent.ToolOutput.RetainedWindow :=
    { firstOffset := firstOffset
    , retainedLen := retainedLen
    , totalBytes := totalBytes
    }
  let slice := Subagent.ToolOutput.readSlice window offset maxBytes
  { name := name
  , firstOffset := firstOffset
  , retainedLen := retainedLen
  , totalBytes := totalBytes
  , offset := offset
  , maxBytes := maxBytes
  , start := slice.start
  , sliceLen := slice.sliceLen
  , nextOffset := slice.nextOffset
  , firstAvailableOffset := slice.firstAvailableOffset
  , totalBytesOut := slice.totalBytes
  , hasMore := slice.hasMore
  , theoremName := theoremName
  }

def toolOutputPagingCases : List ToolOutputPagingCase :=
  [ toolOutputPagingCase "paging_head_page" 0 8 8 0 4
      "Subagent.ToolOutput.readSlice_contiguous_from_live_cursor"
  , toolOutputPagingCase "paging_continuation_no_gap" 0 8 8 4 4
      "Subagent.ToolOutput.readSlice_contiguous_from_live_cursor"
  , toolOutputPagingCase "paging_evicted_prefix_detectable" 6 4 10 0 8
      "Subagent.ToolOutput.readSlice_eviction_detectable"
  , toolOutputPagingCase "paging_cursor_past_end_parks" 0 4 4 9 4
      "Subagent.ToolOutput.readSlice_past_end_empty"
  , toolOutputPagingCase "paging_mid_window_bounded_budget" 2 5 7 3 2
      "Subagent.ToolOutput.readSlice_progress"
  ]

/-- Pinned expected outputs: fails at Lean build time if `readSlice` drifts,
    keeping the emitted rows honest rather than self-referential. -/
theorem toolOutputPagingCases_pinned :
    toolOutputPagingCases.map
        (fun witness =>
          (witness.name, witness.start, witness.sliceLen, witness.nextOffset,
            witness.firstAvailableOffset, witness.totalBytesOut,
            witness.hasMore)) =
      [ ("paging_head_page", 0, 4, 4, 0, 8, true)
      , ("paging_continuation_no_gap", 4, 4, 8, 0, 8, false)
      , ("paging_evicted_prefix_detectable", 6, 4, 10, 6, 10, false)
      , ("paging_cursor_past_end_parks", 4, 0, 4, 0, 4, false)
      , ("paging_mid_window_bounded_budget", 3, 2, 5, 2, 7, true)
      ] := by
  rfl

/-- The two contiguous pages tile the window with no gap and no overlap. -/
theorem toolOutputPagingCases_head_and_continuation_tile :
    ∀ head ∈ toolOutputPagingCases.filter
        (fun witness => witness.name = "paging_head_page"),
      ∀ next ∈ toolOutputPagingCases.filter
          (fun witness => witness.name = "paging_continuation_no_gap"),
        head.nextOffset = next.offset ∧ head.nextOffset = next.start := by
  native_decide

def r6BackgroundTheoremWitnesses : List BackgroundTheoremWitness :=
  [ { theoremName := "Subagent.BridgedState.backgrounded_budget_bounded"
    , witnessKind := "state_invariant"
    , scenario := "background_tool_admission_respects_max_backgrounded_per_parent"
    , numericBound := Subagent.maxBackgroundedPerParent
    , kindFields :=
        [ ("await_mode", "background")
        , ("cancel_policy", "cascade")
        , ("error_code_on_violation", "background_tool_budget_exceeded")
        ]
    }
  , { theoremName := "Subagent.BridgedState.cascade_cancels_child"
    , witnessKind := "reachability_trace"
    , scenario := "parent_terminal_with_cascade_bridge_interrupts_processing_child"
    , numericBound := 2
    , kindFields :=
        [ ("cancel_policy", "cascade")
        , ("child_pre_state", "processing")
        , ("child_pre_admission", "executing")
        , ("child_post_state", "interrupted")
        ]
    }
  ]

end Conformance.ContractCases
