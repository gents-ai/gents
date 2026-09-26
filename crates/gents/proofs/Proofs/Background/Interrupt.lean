import Proofs.Background.Properties.Cancellation
import Proofs.ToolExecution.Transition

/-!
# Foreground Interrupt Scope

An interrupt stops one thread's foreground turn. Every tool call owned by the
interrupted request receives exactly one disposition, and none of them reaches
background work or subagents:

* a pending intent was never dispatched and is cancelled;
* a running foreground native call is cancelled with cause `interrupted`;
* a running awaited subagent bridge becomes background work and stays attached
  to its child, whose completion is later delivered to the session;
* running background work (native processes and background bridges) and
  terminal rows are untouched.

The disposition ignores `cancelPolicy`. That policy only decides whether an
explicit cancellation of a bridge reaches its child
(`BridgedState.Transition.bridge_cancel_cascade`); an interrupt never cancels a
bridge, so it never cascades.
-/

namespace Subagent
namespace Interrupt

open ToolExecution

inductive Disposition where
  | cancel
  | background
  | retain
  deriving DecidableEq, Repr

namespace Disposition

def toContract : Disposition → String
  | .cancel => "cancel"
  | .background => "background"
  | .retain => "retain"

end Disposition

def disposition (c : ToolCallContext) : Disposition :=
  if c.state = .pending then
    .cancel
  else if c.state = .running ∧ c.awaitMode = .foreground then
    if c.childRequestId.isSome then .background else .cancel
  else
    .retain

def interruptTool (c : ToolCallContext) : ToolCallContext :=
  match disposition c with
  | .cancel => { c with state := .cancelled }
  | .background => { c with awaitMode := .background }
  | .retain => c

theorem disposition_ignores_cancel_policy
    (c : ToolCallContext) (policy : CancelPolicy) :
    disposition { c with cancelPolicy := policy } = disposition c := by
  simp [disposition]

/-- Each disposition is the identity or one existing single-row transition:
`cancelBeforeDispatch`/`cancelDuringRun` with cause `interrupted`, or the
foreground-to-background mode flip. -/
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
      by_cases h_child : c.childRequestId.isSome
      · have h : interruptTool c = { c with awaitMode := .background } := by
          simp [interruptTool, disposition, h_pending, h_fg, h_child]
        rw [h]
        exact ToolCallContext.Transition.background h_fg.1 h_fg.2 rfl
      · have h : interruptTool c = { c with state := .cancelled } := by
          simp [interruptTool, disposition, h_pending, h_fg, h_child]
        rw [h]
        exact ToolCallContext.Transition.cancelDuringRun .interrupted h_fg.1 rfl
    · left
      simp [interruptTool, disposition, h_pending, h_fg]

theorem interruptTool_preserves_identity (c : ToolCallContext) :
    (interruptTool c).callId = c.callId ∧
      (interruptTool c).requestId = c.requestId ∧
      (interruptTool c).childRequestId = c.childRequestId ∧
      (interruptTool c).cancelPolicy = c.cancelPolicy := by
  unfold interruptTool
  split <;> simp

/-- Running background work is untouched by an interrupt. -/
theorem interrupt_retains_running_background
    (c : ToolCallContext)
    (h_running : c.state = .running)
    (h_background : c.awaitMode = .background) :
    interruptTool c = c := by
  simp [interruptTool, disposition, h_running, h_background]

/-- A running subagent bridge, awaited or not, keeps running as background
work attached to the same child with the same cancellation policy. -/
theorem interrupt_keeps_running_bridge
    (c : ToolCallContext)
    (h_running : c.state = .running)
    (h_child : c.childRequestId.isSome) :
    (interruptTool c).state = .running ∧
      (interruptTool c).awaitMode = .background ∧
      (interruptTool c).childRequestId = c.childRequestId ∧
      (interruptTool c).cancelPolicy = c.cancelPolicy := by
  cases h_mode : c.awaitMode <;>
    simp [interruptTool, disposition, h_running, h_child, h_mode]

/-- The interrupt stops the thread's own foreground calls. -/
theorem interrupt_cancels_running_foreground_native
    (c : ToolCallContext)
    (h_running : c.state = .running)
    (h_foreground : c.awaitMode = .foreground)
    (h_native : c.childRequestId = none) :
    (interruptTool c).state = .cancelled := by
  simp [interruptTool, disposition, h_running, h_foreground, h_native]

/-- The parent side of an interrupt: every owned tool takes its disposition.
The child request is not touched. -/
def interruptParent (s : BridgedState) : BridgedState :=
  { s with parent := { s.parent with tools := s.parent.tools.map interruptTool } }

theorem interruptParent_child_eq (s : BridgedState) :
    (interruptParent s).child = s.child := rfl

/-- Interrupting the parent of a running bridge leaves no cascade step: the
bridge is still running afterwards, and cascade requires an explicitly
cancelled bridge. The parent request's own lifecycle state is irrelevant. -/
theorem interrupted_parent_admits_no_cascade
    (s post : BridgedState)
    (h_bridge : ∀ t ∈ s.parent.tools, t.callId = s.bridgeCallId →
                  t.state = .running ∧ t.childRequestId.isSome) :
    ¬ BridgedState.BridgeCancelCascadeStep (interruptParent s) post := by
  intro h_step
  obtain ⟨t', h_in', h_id', h_cancelled⟩ := h_step.h_bridge_cancelled
  simp only [interruptParent, List.mem_map] at h_in'
  obtain ⟨t, h_in, rfl⟩ := h_in'
  have h_id : t.callId = s.bridgeCallId :=
    (interruptTool_preserves_identity t).1 ▸ h_id'
  obtain ⟨h_running, h_child⟩ := h_bridge t h_in h_id
  rw [(interrupt_keeps_running_bridge t h_running h_child).1] at h_cancelled
  cases h_cancelled

end Interrupt
end Subagent
