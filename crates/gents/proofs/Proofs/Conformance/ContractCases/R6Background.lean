import Proofs.Background.State
import Proofs.Background.ToolOutput
import Proofs.Conformance.ContractCases.Types
import Proofs.ToolExecution.Executable

/-!
# R6 Tool Backgrounding Conformance Cases

Finite witnesses for the Rust R6 implementation. These rows pin the operator
allowlist budget, Tool-kind bridge terminal projection, restart recovery
terminalization, completion delivery without request creation, and legacy
queue-source parsing during the Subagent→Background rename.
-/

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
      "tool_kind_bridge_failure_cancelled_projects_parent_cancelled"
      "bridge_failure"
      r6NativeToolFixture
      (.cancelDuringRun .interrupted)
      none
      (some "parent_cancelled")
  , r6Case
      "background_recovery_running_live_parent_to_cancelled"
      "recovery"
      "TerminalizeBackgroundedAsInterrupted"
      true
      1
      "cancelled"
      none
      (some "interrupted_on_restart")
  , r6Case
      "background_completion_notification_creates_no_request"
      "completion_delivery"
      "append_notification_without_request"
      true
      1
      "completed"
      (some "done")
  , r6Case
      "legacy_subagent_completion_source_aliases_canonical_key"
      "queue_source"
      "parse_legacy_subagent_completion"
      true
      1
      "completed"
      (some "done")
      none
      none
      (some "subagent_completion")
      (some "background_completion:900")
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
      , ("tool_kind_bridge_failure_cancelled_projects_parent_cancelled",
          true, "background", none, "cancelled", none, none)
      , ("background_recovery_running_live_parent_to_cancelled", true,
          "background", none, "cancelled", none, none)
      , ("background_completion_notification_creates_no_request", true,
          "background", none, "completed", none, none)
      , ("legacy_subagent_completion_source_aliases_canonical_key", true,
          "background", none, "completed", some "subagent_completion",
          some "background_completion:900")
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
