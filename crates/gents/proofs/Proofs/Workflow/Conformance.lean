import Proofs.Workflow.FanOut
import Proofs.Workflow.CompositeInterrupt

/-!
# Workflow conformance witnesses

Finite witness rows for the Rust barrier-projection fence and composite
interrupt cleanup fence (#837).

Every `legal` annotation below is *entailed by the model*, not hand-asserted:
`workflowCasesLegalCorrect` decides that each row's `legal` equals the computable
barrier predicate `barrierLegal` applied to that row's fields. A wrong annotation
fails to build.

`barrierLegal` is the computable mirror of the Rust
`workflow_barrier_projection_legal`: a group is legal iff it is non-empty AND
(synthesis is absent OR every fan-out bridge is terminal).

Composite interrupt cases pin the post-cleanup invariant: after bounded
interrupt cleanup (or terminal-parent recovery), the outer composite is not
eligible as active and carries a consistent interrupted cancel cause when it
was still running.
-/

namespace Workflow
namespace Conformance

open ToolExecution
open CompositeInterrupt
open CompositeInterrupt.State
open RequestState

structure BarrierCase where
  name : String
  groupTerminalStates : List ToolCallState
  synthesisPresent : Bool
  legal : Bool

/-- Computable barrier-legality predicate, definitionally aligned with the Rust
    `workflow_barrier_projection_legal(states, synthesis_present)`:
    `non-empty ∧ (¬synthesis_present ∨ states.all isTerminal)`.

    The `states.all isTerminal` clause is exactly `WorkflowGroup.allTerminalB`'s
    body; the terminal set is `{completed, failed, timedOut, cancelled}` via the
    `HasTerminal ToolCallState` instance, matching
    `WORKFLOW_TERMINAL_TOOL_STATES` on the Rust side. -/
def barrierLegal (states : List ToolCallState) (synthesisPresent : Bool) : Bool :=
  !states.isEmpty &&
    (!synthesisPresent || states.all (fun s => decide (isTerminal s)))

/-- Each witness's hand-written `legal` matches the computable predicate applied
    to its own fields. -/
def caseLegalCorrect (c : BarrierCase) : Bool :=
  c.legal == barrierLegal c.groupTerminalStates c.synthesisPresent

def workflowCases : List BarrierCase :=
  [ { name := "all_terminal_then_synthesis"
    , groupTerminalStates := [.completed, .completed, .cancelled]
    , synthesisPresent := true
    , legal := true
    }
  , { name := "failed_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .failed, .completed]
    , synthesisPresent := true
    , legal := true
    }
  , { name := "pending_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .running, .completed]
    , synthesisPresent := true
    , legal := false
    }
  , { name := "empty_group"
    , groupTerminalStates := []
    , synthesisPresent := true
    , legal := false
    }
    -- conf-3 (a): pre-barrier branch — a non-terminal sibling is legal as long
    -- as synthesis has NOT been spawned. Fences the Rust predicate's
    -- `!synthesis_present` short-circuit (and the `synthesis_present := false`
    -- serializer path).
  , { name := "running_sibling_no_synthesis"
    , groupTerminalStates := [.completed, .running, .completed]
    , synthesisPresent := false
    , legal := true
    }
    -- conf-3 (b): all-terminal INCLUDING a `.timedOut` sibling + synthesis.
    -- Fences the camelCase `timedOut` terminal-vocabulary string on both sides.
  , { name := "timed_out_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .timedOut, .cancelled]
    , synthesisPresent := true
    , legal := true
    }
  ]

/-- **Conformance lemma (conf-2).** Every entry of `workflowCases` carries a
    `legal` value entailed by the computable barrier predicate. Decided by
    `native_decide`, so a wrong `legal` annotation fails to build. -/
theorem workflowCasesLegalCorrect :
    workflowCases.all caseLegalCorrect = true := by
  native_decide

/-! ## Composite interrupt cleanup cases (#837) -/

structure CompositeInterruptCase where
  name : String
  phase : Phase
  parentState : RequestState
  outerState : ToolCallState
  outerCancelCause : Option CancelCause
  fanOutBridges : List ToolCallState
  synthesisBridge : Option ToolCallState
  continuationOwned : Bool
  pendingChildCleanup : Bool
  /-- Expected post-cleanup outer eligible-active flag. -/
  postOuterEligibleActive : Bool
  /-- Expected post-cleanup outer state. -/
  postOuterState : ToolCallState
  /-- Expected post-cleanup cancel cause (None when outer was already terminal
      non-cancel). -/
  postOuterCancelCause : Option CancelCause
  /-- Expected post-cleanup continuation ownership. -/
  postContinuationOwned : Bool
  deriving Repr

/-- Build a pre-state from a case row. -/
def caseToState (c : CompositeInterruptCase) : State :=
  { parentState := c.parentState
  , outerState := c.outerState
  , outerCancelCause := c.outerCancelCause
  , phase := c.phase
  , fanOutBridges := c.fanOutBridges
  , synthesisBridge := c.synthesisBridge
  , continuationOwned := c.continuationOwned
  , pendingChildCleanup := c.pendingChildCleanup }

/-- Apply the cleanup transition appropriate to the pre-state:
    - parent processing/interrupted → interruptCleanupPost
    - parent otherwise terminal with running outer → recoverTerminalParentPost
    - else identity (should not appear in witnesses). -/
def applyCleanup (s : State) : State :=
  if s.parentState = .processing || s.parentState = .interrupted then
    interruptCleanupPost s
  else if isTerminal s.parentState && s.outerState = .running then
    recoverTerminalParentPost s
  else
    s

def casePostCorrect (c : CompositeInterruptCase) : Bool :=
  let post := applyCleanup (caseToState c)
  decide (post.outerEligibleActive) == c.postOuterEligibleActive &&
  post.outerState == c.postOuterState &&
  post.outerCancelCause == c.postOuterCancelCause &&
  post.continuationOwned == c.postContinuationOwned &&
  decide (cleanupInvariant post)

def mkInterruptCase
    (name : String)
    (phase : Phase)
    (fanOut : List ToolCallState)
    (synthesis : Option ToolCallState)
    : CompositeInterruptCase :=
  { name := name
  , phase := phase
  , parentState := .processing
  , outerState := .running
  , outerCancelCause := none
  , fanOutBridges := fanOut
  , synthesisBridge := synthesis
  , continuationOwned := true
  , pendingChildCleanup := false
  , postOuterEligibleActive := false
  , postOuterState := .cancelled
  , postOuterCancelCause := some .interrupted
  , postContinuationOwned := false }

def compositeInterruptCases : List CompositeInterruptCase :=
  [ mkInterruptCase "interrupt_during_fan_out_spawn" .fanOutSpawn
      [.running, .running] none
  , mkInterruptCase "interrupt_during_fan_out_barrier" .fanOutBarrier
      [.completed, .running] none
  , mkInterruptCase "interrupt_between_barrier_and_synthesis" .synthesisSpawn
      [.completed, .completed, .cancelled] none
  , mkInterruptCase "interrupt_during_synthesis_run" .synthesisRun
      [.completed, .completed] (some .running)
  , mkInterruptCase "interrupt_during_result_persist" .resultPersist
      [.completed, .completed] (some .completed)
  , -- Duplicate interrupt: parent already interrupted, outer already cancelled.
    { name := "duplicate_interrupt_delivery"
    , phase := .fanOutBarrier
    , parentState := .interrupted
    , outerState := .cancelled
    , outerCancelCause := some .interrupted
    , fanOutBridges := [.cancelled, .cancelled]
    , synthesisBridge := none
    , continuationOwned := false
    , pendingChildCleanup := true
    , postOuterEligibleActive := false
    , postOuterState := .cancelled
    , postOuterCancelCause := some .interrupted
    , postContinuationOwned := false }
  , -- Restart/live recovery: terminal parent, running outer composite.
    { name := "recover_terminal_parent_running_outer"
    , phase := .fanOutBarrier
    , parentState := .interrupted
    , outerState := .running
    , outerCancelCause := none
    , fanOutBridges := [.cancelled, .running]
    , synthesisBridge := none
    , continuationOwned := false
    , pendingChildCleanup := false
    , postOuterEligibleActive := false
    , postOuterState := .cancelled
    , postOuterCancelCause := some .interrupted
    , postContinuationOwned := false }
  , -- Recovery when children already terminal but outer still running and the
    -- parent is non-interrupt terminal (failed) → outer fails (not cancelled).
    { name := "recover_terminal_children_running_outer"
    , phase := .resultPersist
    , parentState := .failed
    , outerState := .running
    , outerCancelCause := none
    , fanOutBridges := [.completed, .completed]
    , synthesisBridge := some .completed
    , continuationOwned := false
    , pendingChildCleanup := false
    , postOuterEligibleActive := false
    , postOuterState := .failed
    , postOuterCancelCause := none
    , postContinuationOwned := false }
  ]

theorem compositeInterruptCasesCorrect :
    compositeInterruptCases.all casePostCorrect = true := by
  native_decide

end Conformance
end Workflow
