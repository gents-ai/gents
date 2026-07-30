import Proofs.Workflow.FanOut
import Proofs.Workflow.CompositeInterrupt

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

def barrierLegal (states : List ToolCallState) (synthesisPresent : Bool) : Bool :=
  !states.isEmpty &&
    (!synthesisPresent || states.all (fun s => decide (isTerminal s)))

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
  , { name := "running_sibling_no_synthesis"
    , groupTerminalStates := [.completed, .running, .completed]
    , synthesisPresent := false
    , legal := true
    }
  , { name := "timed_out_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .timedOut, .cancelled]
    , synthesisPresent := true
    , legal := true
    }
  ]

theorem workflowCasesLegalCorrect :
    workflowCases.all caseLegalCorrect = true := by
  native_decide

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
  postOuterEligibleActive : Bool
  postOuterState : ToolCallState
  postOuterCancelCause : Option CancelCause
  postContinuationOwned : Bool
  deriving Repr

def caseToState (c : CompositeInterruptCase) : State :=
  { parentState := c.parentState
  , outerState := c.outerState
  , outerCancelCause := c.outerCancelCause
  , phase := c.phase
  , fanOutBridges := c.fanOutBridges
  , synthesisBridge := c.synthesisBridge
  , continuationOwned := c.continuationOwned
  , pendingChildCleanup := c.pendingChildCleanup }

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
  ,
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
  ,
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
  ,
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
