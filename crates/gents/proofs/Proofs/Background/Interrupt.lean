import Proofs.ToolExecution.Transition

/-!
# Foreground Interrupt Scope

An interrupt stops one thread's foreground turn: a pending intent is cancelled,
a running foreground call is cancelled with cause `interrupted`, and running
background work and terminal rows are untouched.
-/

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

theorem interrupt_cancels_running_foreground
    (c : ToolCallContext)
    (h_running : c.state = .running)
    (h_foreground : c.awaitMode = .foreground) :
    (interruptTool c).state = .cancelled := by
  simp [interruptTool, disposition, h_running, h_foreground]

theorem interrupt_never_backgrounds (c : ToolCallContext) :
    (interruptTool c).awaitMode = c.awaitMode :=
  (interruptTool_preserves_identity c).2.2

end Interrupt
end Background
