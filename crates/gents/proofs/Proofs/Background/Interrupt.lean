import Proofs.ToolExecution.Transition

/-!
# Foreground Interrupt Scope

An interrupt stops one thread's foreground turn. Every tool call owned by the
interrupted request receives exactly one disposition, and none of them reaches
background work or another session:

* a pending intent was never dispatched and is cancelled;
* a running foreground call is cancelled with cause `interrupted`;
* running background work (native processes and `create_session` /
  `send_message` rows) and terminal rows are untouched.
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

/-- The per-row interrupt disposition. There is no cascade: interrupting a
session stops only that thread's foreground work. A session it started with
`create_session` is an ordinary agent's session, addressed directly, and keeps
running; only `cancel_process` or the UI kill on that one background row
interrupts that one request. Jack's 0.20 decision: the runtime encodes no
hierarchy between agents, so no parent fate is a cancel signal for another
session, and any stronger policy belongs to the application. -/
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

/-- Running background work, including every started session's row, is
untouched by an interrupt. -/
theorem interrupt_retains_running_background
    (c : ToolCallContext)
    (h_running : c.state = .running)
    (h_background : c.awaitMode = .background) :
    interruptTool c = c := by
  simp [interruptTool, disposition, h_running, h_background]

/-- The interrupt stops the thread's own foreground calls. -/
theorem interrupt_cancels_running_foreground
    (c : ToolCallContext)
    (h_running : c.state = .running)
    (h_foreground : c.awaitMode = .foreground) :
    (interruptTool c).state = .cancelled := by
  simp [interruptTool, disposition, h_running, h_foreground]

/-- An interrupt never produces a running background row it did not already
have: no work is moved out of the interrupted thread. -/
theorem interrupt_never_backgrounds (c : ToolCallContext) :
    (interruptTool c).awaitMode = c.awaitMode :=
  (interruptTool_preserves_identity c).2.2

end Interrupt
end Background
