import Proofs.Background.State
import Proofs.Background.ToolOutput
import Proofs.Conformance.ContractCases.Types

/-!
# R6 Tool Backgrounding Conformance Cases

Finite witnesses for the Rust R6 implementation. These rows pin the operator
allowlist budget, Tool-kind bridge terminal projection, restart recovery
terminalization, and queue-source vocabulary during the Subagent→Background
rename.
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

def r6BackgroundingCases : List R6BackgroundingCase :=
  [ r6Case
      "background_tool_budget_count_7_admits_spawn"
      "budget"
      "spawn_process"
      true
      7
      "running"
  , r6Case
      "background_tool_budget_count_8_rejects_spawn"
      "budget"
      "spawn_process"
      false
      8
      "rejected"
      none
      none
      (some "background_tool_budget_exceeded")
  , r6Case
      "tool_kind_bridge_complete_persists_result"
      "bridge"
      "bridge_complete"
      true
      1
      "completed"
      (some "done")
  , r6Case
      "tool_kind_bridge_failure_cancelled_projects_parent_cancelled"
      "bridge"
      "bridge_failure"
      true
      1
      "cancelled"
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
      "background_completion_source_writes_canonical_key"
      "queue_source"
      "enqueue_background_completion"
      true
      1
      "completed"
      (some "done")
      none
      none
      (some "background_completion")
      (some "background_completion:900")
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
