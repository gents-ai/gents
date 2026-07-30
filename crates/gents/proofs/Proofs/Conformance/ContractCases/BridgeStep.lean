import Proofs.Background.Executable
import Proofs.Conformance.ContractCases.Types

/-!
# Executable Bridge-Step Conformance Cases (#937)

Finite witnesses computed by **running** `Subagent.BridgedState.step` on
concrete subagent-bridge fixtures. Unlike the earlier hand-authored rows,
these cannot drift from the model silently: changing a step guard or a
projected post-state changes the emitted JSON and fails the Rust consumer,
which drives the same shapes through
`project_background_subagent_completion` (complete/failure guards live
there — Rust `bridge_complete` itself is a trust boundary) and
`ToolCallLifecycle::bridge_cancel_cascade`.
-/

namespace Conformance.ContractCases

open Subagent

/-- The bridge tool row on the parent: running, background, child-linked. -/
def bridgeStepToolRow
    (policy : CancelPolicy)
    (persistence : PersistenceState) : ToolExecution.ToolCallContext :=
  { callId := 77
  , requestId := 900
  , state := .running
  , operation := .nativeCommand
  , deadline := 100
  , startedAt := some 1
  , currentTime := 10
  , failureClass := none
  , persistence := persistence
  , approval := none
  , awaitMode := .background
  , cancelPolicy := policy
  , childRequestId := some 901
  }

def bridgeStepRequest
    (state : RequestState) (admission : AdmissionState) : RequestContext :=
  { state := state
  , origin := .interactive
  , backend := { val := "backend-a" }
  , admission := admission
  , deadline := 100
  , requestDeadline := none
  , claimTime := 0
  , currentTime := 10
  , retryCount := 0
  , maxRetries := 3
  , progressSeq := 0
  , messageSeq := 0
  , isLatest := true
  , persistence := .committed
  , interruptRequestedAt := none
  , validUntil := none
  , subagentDepth := 0
  , causedByParentRequestId := none
  , causedByParentToolCallId := none
  }

/-- Admission coherent with each fixture request state. -/
def bridgeStepAdmission : RequestState → AdmissionState
  | .processing => .executing
  | .pending => .released
  | _ => .released

def bridgeStepComposed
    (rid : RequestId) (req : RequestContext)
    (tools : List ToolExecution.ToolCallContext) : ComposedState :=
  { requestId := rid
  , process := .ready
  , request := req
  , call := { callId := 1, requestId := rid, backend := { val := "backend-a" }, state := .completed }
  , tools := tools
  }

/-- One concrete bridged fixture: parent at `parentState` carrying the
    bridge row, child at `childState` with spawn lineage. -/
def bridgeStepFixture
    (childState parentState : RequestState)
    (policy : CancelPolicy)
    (bridgeCommitted : Bool) : Subagent.BridgedState :=
  let childReq :=
    { bridgeStepRequest childState (bridgeStepAdmission childState) with
        subagentDepth := 1
      , causedByParentRequestId := some 900
      , causedByParentToolCallId := some 77 }
  let child := bridgeStepComposed 901 childReq []
  { parent :=
      bridgeStepComposed 900
        (bridgeStepRequest parentState (bridgeStepAdmission parentState))
        [ bridgeStepToolRow policy
            (if bridgeCommitted then .committed else .committing) ]
  , child := child
  , secondLeg := .subagent child
  , bridgeCallId := 77
  }

def bridgeStepCase
    (name : String)
    (event : Subagent.BridgedState.Event)
    (eventName : String)
    (childState parentState : RequestState)
    (policy : CancelPolicy)
    (bridgeCommitted : Bool)
    (theoremName : String) : BridgeStepCase :=
  let fixture := bridgeStepFixture childState parentState policy bridgeCommitted
  let base : BridgeStepCase :=
    { name := name
    , event := eventName
    , childState := childState.toDefraDB
    , parentState := parentState.toDefraDB
    , cancelPolicy := policy.toDefraDB
    , bridgeCommitted := bridgeCommitted
    , legal := false
    , postToolState := none
    , postChildInterruptSet := false
    , theoremName := theoremName
    }
  match Subagent.BridgedState.step fixture event with
  | none => base
  | some post =>
      { base with
          legal := true
        , postToolState :=
            (post.parent.findToolByCallId 77).map fun t => t.state.toDefraDB
        , postChildInterruptSet := post.child.request.interruptRequestedAt.isSome
      }

def bridgeStepCases : List BridgeStepCase :=
  [ bridgeStepCase "bridge_step_complete_child_completed"
      .bridge_complete "bridge_complete" .completed .processing .cascade true
      "Subagent.BridgedState.step_refines_transition"
  , bridgeStepCase "bridge_step_complete_child_still_processing"
      .bridge_complete "bridge_complete" .processing .processing .cascade true
      "Subagent.BridgedState.step_refines_transition"
  , bridgeStepCase "bridge_step_complete_uncommitted_bridge_row"
      .bridge_complete "bridge_complete" .completed .processing .cascade false
      "Subagent.BridgedState.step_refines_transition"
  , bridgeStepCase "bridge_step_failure_child_interrupted"
      .bridge_failure "bridge_failure" .interrupted .processing .cascade true
      "Subagent.BridgedState.step_refines_transition"
  , bridgeStepCase "bridge_step_failure_child_failed"
      .bridge_failure "bridge_failure" .failed .processing .cascade true
      "Subagent.BridgedState.step_refines_transition"
  , bridgeStepCase "bridge_step_failure_child_dead"
      .bridge_failure "bridge_failure" .dead .processing .cascade true
      "Subagent.BridgedState.step_refines_transition"
  , bridgeStepCase "bridge_step_failure_child_completed_rejected"
      .bridge_failure "bridge_failure" .completed .processing .cascade true
      "Subagent.BridgedState.step_refines_transition"
  , -- Cascade rows: `post_tool_state = "running"` is the MODEL's
    -- structurally-inert parent (the cascade step only latches the child's
    -- interrupt flag). The Rust decision seam (`bridge_cancel_cascade`)
    -- implements the tool-cancelled arm of the guard, so the runtime driver
    -- reaches it via `cancel_during_run` first and binds only the
    -- intent/interrupt outcome — the "running" post is pinned by the Lean
    -- `rfl` theorem, not by the runtime.
    bridgeStepCase "bridge_step_cascade_parent_interrupted_cascade"
      .bridge_cancel_cascade "bridge_cancel_cascade"
      .processing .interrupted .cascade true
      "Subagent.BridgedState.cascade_cancels_child"
  , bridgeStepCase "bridge_step_cascade_parent_interrupted_detach"
      .bridge_cancel_cascade "bridge_cancel_cascade"
      .processing .interrupted .detach true
      "Subagent.BridgedState.detach_does_not_cancel_child"
  , bridgeStepCase "bridge_step_cascade_parent_live_rejected"
      .bridge_cancel_cascade "bridge_cancel_cascade"
      .processing .processing .cascade true
      "Subagent.BridgedState.step_refines_transition"
  ]

/-- Pinned outcomes: fails at Lean build time if a step guard or projection
    drifts, keeping the emitted rows honest rather than self-referential. -/
theorem bridgeStepCases_pinned :
    bridgeStepCases.map
        (fun witness =>
          (witness.name, witness.legal, witness.postToolState,
            witness.postChildInterruptSet)) =
      [ ("bridge_step_complete_child_completed", true, some "completed", false)
      , ("bridge_step_complete_child_still_processing", false, none, false)
      , ("bridge_step_complete_uncommitted_bridge_row", false, none, false)
      , ("bridge_step_failure_child_interrupted", true, some "cancelled", false)
      , ("bridge_step_failure_child_failed", true, some "failed", false)
      , ("bridge_step_failure_child_dead", true, some "failed", false)
      , ("bridge_step_failure_child_completed_rejected", false, none, false)
      , ("bridge_step_cascade_parent_interrupted_cascade", true, some "running",
          true)
      , ("bridge_step_cascade_parent_interrupted_detach", false, none, false)
      , ("bridge_step_cascade_parent_live_rejected", false, none, false)
      ] := by
  rfl

end Conformance.ContractCases
