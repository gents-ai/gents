import Proofs.Runtime.CrossMachineComposed.Reachability
import Proofs.Conformance.Family

namespace Conformance.ComposedTermination

open Lean ComposedState ComposedState.ReachabilityWitness

structure Witness where
  scenario : String
  state : ComposedState
  tool : ToolExecution.ToolCallContext

/-- Reachable compositions with one linked tool whose request stops: a pending
and a running tool past the request deadline, and a pending and a running tool
of an interrupted request. -/
def witnesses : List Witness :=
  [ ⟨"pending_tool_deadline_exceeded_cancels_before_dispatch", withExpiredPendingTool, expiredPendingTool⟩
  , ⟨"running_tool_deadline_exceeded_times_out_on_recovery", withExpiredRunningTool, expiredRunningTool⟩
  , ⟨"interrupted_request_cancels_live_pending_tool", interruptedWithTool, pendingTool⟩
  , ⟨"interrupted_request_cancels_live_running_tool", interruptedWithRunningTool, runningTool⟩ ]

structure Input where
  scenario : String
  toolPreState : String
  deadlineExceeded : Bool
  toolCallId : Nat
  deriving ToJson

structure Output where
  preRequestState : String
  toolPostState : Option String
  cancelCause : Option String
  toolFailureClass : Option String
  deriving ToJson

def witnessOf (scenario : String) : Option Witness :=
  witnesses.find? (·.scenario == scenario)

@[conformance_family]
def linked : Family Input Output where
  inputs := witnesses.map fun w =>
    ⟨w.scenario, w.tool.state.toDefraDB, decide w.state.request.deadlineExceeded, w.tool.callId⟩
  model := fun i => match witnessOf i.scenario with
    | none => ⟨"", none, none, none⟩
    | some w =>
      let termination := linkedTermination w.state.request w.tool
      { preRequestState := w.state.request.state.toDefraDB
        toolPostState := termination.map (·.state.toDefraDB)
        cancelCause := termination.bind (·.cancelCause.map ToolExecution.CancelCause.toDefraDB)
        toolFailureClass :=
          termination.bind (·.failureClass.map ToolExecution.FailureClass.toDefraDB) }

example : witnesses.all (fun w => decide (w.tool ∈ w.state.tools) && !decide (IsDetached w.tool)) = true := by
  native_decide

example : witnesses.all (fun w =>
    decide w.state.request.deadlineExceeded == decide w.tool.deadlineExceeded) = true := by
  native_decide

example : witnesses.all (fun w =>
    decide (w.state.request.admission = .executing ∧ w.state.request.state = .processing) ||
      decide (w.state.request.admission = .released ∧ w.state.request.state = .interrupted)) = true := by
  native_decide

end Conformance.ComposedTermination
