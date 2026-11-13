import Proofs.Session.ToolExecution.Transition
namespace Background
namespace Interrupt

open ToolExecution

inductive Disposition where
  | cancel
  | retain
  deriving DecidableEq, Repr

namespace Disposition

def toContract : Disposition → String
  | .cancel => "cancel"
  | .retain => "retain"

end Disposition

/-- No cascade: interrupting a session stops only that thread's foreground work;
`agent_new` sessions keep running, addressed directly. Any stronger policy
belongs to the application. -/
def disposition (c : ToolCallContext) : Disposition :=
  if c.state = .pending then
    .cancel
  else if c.state = .running ∧ c.awaitMode = .foreground then
    .cancel
  else
    .retain

def interruptTool (c : ToolCallContext) : ToolCallContext :=
  match disposition c with
  | .cancel => { c with state := .cancelled }
  | .retain => c

/-- Each disposition is the identity or one existing single-row transition:
`cancelBeforeDispatch`/`cancelDuringRun` with cause `interrupted`. -/
theorem interruptTool_refines (c : ToolCallContext) :
    interruptTool c = c ∨ ToolCallContext.Transition c (interruptTool c) := by
  by_cases h_pending : c.state = .pending
  · right
    have h : interruptTool c = { c with state := .cancelled } := by
      simp [interruptTool, disposition, h_pending]
    rw [h]
    exact ToolCallContext.Transition.cancelBeforeDispatch .interrupted h_pending rfl
  · by_cases h_fg : c.state = .running ∧ c.awaitMode = .foreground
    · right
      have h : interruptTool c = { c with state := .cancelled } := by
        simp [interruptTool, disposition, h_pending, h_fg]
      rw [h]
      exact ToolCallContext.Transition.cancelDuringRun .interrupted h_fg.1 rfl
    · left
      simp [interruptTool, disposition, h_pending, h_fg]

theorem interruptTool_preserves_identity (c : ToolCallContext) :
    (interruptTool c).callId = c.callId ∧
      (interruptTool c).requestId = c.requestId ∧
      (interruptTool c).awaitMode = c.awaitMode := by
  unfold interruptTool
  split <;> simp

theorem interrupt_retains_running_background
    (c : ToolCallContext)
    (h_running : c.state = .running)
    (h_background : c.awaitMode = .background) :
    interruptTool c = c := by
  simp [interruptTool, disposition, h_running, h_background]

/-- A running plugin job is background work, so interrupting its caller
leaves the job row running as the same job row. -/
theorem interrupt_retains_running_detached_job
    (c : ToolCallContext)
    (h_job : c.operation = .pluginJob)
    (h_running : c.state = .running)
    (h_background : c.awaitMode = .background) :
    interruptTool c = c ∧ (interruptTool c).operation = .pluginJob := by
  have h := interrupt_retains_running_background c h_running h_background
  exact ⟨h, by rw [h]; exact h_job⟩

theorem interrupt_cancels_running_foreground
    (c : ToolCallContext)
    (h_running : c.state = .running)
    (h_foreground : c.awaitMode = .foreground) :
    (interruptTool c).state = .cancelled := by
  simp [interruptTool, disposition, h_running, h_foreground]

theorem interrupt_never_backgrounds (c : ToolCallContext) :
    (interruptTool c).awaitMode = c.awaitMode :=
  (interruptTool_preserves_identity c).2.2

/-- A hook interrupts only the calls it holds in flight. Another hook's calls,
in its own session and request, are not in that set and keep their state. -/
structure HookCalls where
  own : List ToolCallContext
  other : List ToolCallContext
  deriving DecidableEq, Repr

/-- The interrupted hook's calls after the interrupt, the other hook's calls,
and how many running calls the interrupt cancelled. -/
def interruptHook (h : HookCalls) : HookCalls × Nat :=
  ({ own := h.own.map interruptTool, other := h.other },
   (h.own.filter fun c => c.state == .running && (interruptTool c).state == .cancelled).length)

theorem interrupting_a_hook_keeps_other_hooks_calls (h : HookCalls) :
    (interruptHook h).1.other = h.other := rfl

private def liveCall (callId requestId : Nat) : ToolCallContext :=
  { callId, requestId, state := .running, operation := .nativeCommand, deadline := 100
  , startedAt := some 1, currentTime := 10, persistence := .committed, awaitMode := .foreground }

theorem cancelling_one_hook_does_not_cancel_unrelated_live_tool_call :
    let after := interruptHook { own := [liveCall 1 1], other := [liveCall 2 2] }
    after.2 = 1 ∧ (after.1.own.map (·.state)) = [.cancelled] ∧
      (after.1.other.map (·.state)) = [.running] := by decide

end Interrupt
end Background
