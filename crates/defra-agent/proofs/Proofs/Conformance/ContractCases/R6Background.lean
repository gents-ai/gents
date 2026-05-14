import Proofs.Background.State
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
      "background_tool"
      true
      7
      "running"
  , r6Case
      "background_tool_budget_count_8_rejects_spawn"
      "budget"
      "background_tool"
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

end Conformance.ContractCases
