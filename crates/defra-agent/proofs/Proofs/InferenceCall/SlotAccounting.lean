import Proofs.InferenceCall.Properties
import Mathlib.Algebra.BigOperators.Group.Finset.Basic

/-!
# Inference Call Slot Accounting

Production scheduler capacity is reconstructed from persisted `InferenceCall`
rows. A backend slot is held exactly by rows whose `call_state` is `running`.
-/

open scoped BigOperators

namespace InferenceCallState

/-- Whether a persisted call state holds backend capacity. -/
def holdsBackendSlot : InferenceCallState → Prop
  | .running => True
  | .queued => False
  | .cancelled => False
  | .completed => False
  | .failed => False

instance : DecidablePred holdsBackendSlot := by
  intro s
  cases s with
  | queued => exact isFalse (by simp [holdsBackendSlot])
  | running => exact isTrue (by simp [holdsBackendSlot])
  | cancelled => exact isFalse (by simp [holdsBackendSlot])
  | completed => exact isFalse (by simp [holdsBackendSlot])
  | failed => exact isFalse (by simp [holdsBackendSlot])

theorem queued_no_backend_slot :
    ¬ holdsBackendSlot .queued := by
  simp [holdsBackendSlot]

theorem running_holds_backend_slot :
    holdsBackendSlot .running := by
  simp [holdsBackendSlot]

theorem cancelled_no_backend_slot :
    ¬ holdsBackendSlot .cancelled := by
  simp [holdsBackendSlot]

theorem completed_no_backend_slot :
    ¬ holdsBackendSlot .completed := by
  simp [holdsBackendSlot]

theorem failed_no_backend_slot :
    ¬ holdsBackendSlot .failed := by
  simp [holdsBackendSlot]

theorem terminal_no_backend_slot
    {s : InferenceCallState}
    (h_terminal : isTerminal s) :
    ¬ holdsBackendSlot s := by
  cases s <;>
    simp [holdsBackendSlot, HasTerminal.isTerminal, InferenceCallState.instHasTerminal] at h_terminal ⊢

end InferenceCallState

namespace InferenceCall

/-- Whether this persisted row currently holds backend capacity. -/
def holdsBackendSlot (call : InferenceCall) : Prop :=
  InferenceCallState.holdsBackendSlot call.state

instance (call : InferenceCall) : Decidable call.holdsBackendSlot := by
  unfold holdsBackendSlot
  infer_instance

/-- One unit of reconstructed capacity contribution for a backend. -/
def slotContribution (call : InferenceCall) (bid : BackendId) : Nat :=
  if call.backend = bid ∧ call.holdsBackendSlot then 1 else 0

/-- Reconstructed held slots for a backend, counted from persisted call rows. -/
def reconstructedSlotCount
    (callIds : Finset Nat)
    (row : Nat → InferenceCall)
    (bid : BackendId) : Nat :=
  ∑ callId ∈ callIds, (row callId).slotContribution bid

/-- The scheduler's running view is a projection over `InferenceCall` rows. -/
def ReconstructsSchedulerRunning
    (callIds : Finset Nat)
    (row : Nat → InferenceCall)
    (scheduler : SchedulerState) : Prop :=
  ∀ bid : BackendId, scheduler.running bid = reconstructedSlotCount callIds row bid

theorem queued_call_does_not_hold_slot
    {call : InferenceCall}
    (h_state : call.state = .queued) :
    ¬ call.holdsBackendSlot := by
  unfold holdsBackendSlot
  rw [h_state]
  exact InferenceCallState.queued_no_backend_slot

theorem running_call_holds_slot
    {call : InferenceCall}
    (h_state : call.state = .running) :
    call.holdsBackendSlot := by
  unfold holdsBackendSlot
  rw [h_state]
  exact InferenceCallState.running_holds_backend_slot

theorem terminal_call_does_not_hold_slot
    {call : InferenceCall}
    (h_terminal : isTerminal call.state) :
    ¬ call.holdsBackendSlot := by
  unfold holdsBackendSlot
  exact InferenceCallState.terminal_no_backend_slot h_terminal

theorem queued_call_contributes_zero
    {call : InferenceCall} {bid : BackendId}
    (h_state : call.state = .queued) :
    call.slotContribution bid = 0 := by
  unfold slotContribution
  by_cases h : call.backend = bid ∧ call.holdsBackendSlot
  · exact False.elim (queued_call_does_not_hold_slot h_state h.right)
  · simp [h]

theorem running_call_contributes_one
    {call : InferenceCall} {bid : BackendId}
    (h_backend : call.backend = bid)
    (h_state : call.state = .running) :
    call.slotContribution bid = 1 := by
  unfold slotContribution
  have h_slot : call.holdsBackendSlot := running_call_holds_slot h_state
  simp [h_backend, h_slot]

theorem running_call_holds_exactly_one_slot
    {call : InferenceCall} {bid : BackendId}
    (h_backend : call.backend = bid)
    (h_state : call.state = .running) :
    call.slotContribution bid = 1 :=
  running_call_contributes_one h_backend h_state

theorem terminal_call_contributes_zero
    {call : InferenceCall} {bid : BackendId}
    (h_terminal : isTerminal call.state) :
    call.slotContribution bid = 0 := by
  unfold slotContribution
  by_cases h : call.backend = bid ∧ call.holdsBackendSlot
  · exact False.elim (terminal_call_does_not_hold_slot h_terminal h.right)
  · simp [h]

theorem cancelled_call_contributes_zero
    {call : InferenceCall} {bid : BackendId}
    (h_state : call.state = .cancelled) :
    call.slotContribution bid = 0 := by
  apply terminal_call_contributes_zero
  rw [h_state]
  exact Or.inl rfl

theorem completed_call_contributes_zero
    {call : InferenceCall} {bid : BackendId}
    (h_state : call.state = .completed) :
    call.slotContribution bid = 0 := by
  apply terminal_call_contributes_zero
  rw [h_state]
  exact Or.inr (Or.inl rfl)

theorem failed_call_contributes_zero
    {call : InferenceCall} {bid : BackendId}
    (h_state : call.state = .failed) :
    call.slotContribution bid = 0 := by
  apply terminal_call_contributes_zero
  rw [h_state]
  exact Or.inr (Or.inr rfl)

/-- Permit drop writes a terminal row for an already-running call. -/
inductive PermitDropTerminalization : InferenceCall → InferenceCall → Prop where
  | stream_dropped {pre post : InferenceCall} :
      pre.state = .running →
      post = { pre with state := .failed } →
      PermitDropTerminalization pre post
  | interrupted_drop {pre post : InferenceCall} :
      pre.state = .running →
      post = { pre with state := .cancelled } →
      PermitDropTerminalization pre post

theorem permitDrop_terminalization_terminal
    {pre post : InferenceCall}
    (h_drop : PermitDropTerminalization pre post) :
    isTerminal post.state := by
  cases h_drop with
  | stream_dropped _ h_post =>
      rw [h_post]
      exact Or.inr (Or.inr rfl)
  | interrupted_drop _ h_post =>
      rw [h_post]
      exact Or.inl rfl

theorem permitDrop_terminalization_not_counted
    {pre post : InferenceCall}
    (h_drop : PermitDropTerminalization pre post)
    (bid : BackendId) :
    post.slotContribution bid = 0 :=
  terminal_call_contributes_zero (permitDrop_terminalization_terminal h_drop)

theorem live_linked_trace_to_non_slot_holding_terminal
    {call : InferenceCall} {requestId : RequestId}
    (h_linked : call.linkedTo requestId)
    (h_live : call.cancellable) :
    ∃ post : InferenceCall,
      Trace call post ∧
      post.linkedTo requestId ∧
      isTerminal post.state ∧
      (∀ bid : BackendId, post.slotContribution bid = 0) := by
  let post := call.cancel
  have h_trace : Trace call post := live_trace_to_cancelled call h_live
  refine ⟨post, h_trace, ?_, ?_, ?_⟩
  · unfold linkedTo
    change call.cancel.requestId = requestId
    rw [cancel_preserves_requestId call]
    exact h_linked
  · change isTerminal call.cancel.state
    rw [cancel_state call]
    exact Or.inl rfl
  · intro bid
    apply terminal_call_contributes_zero
    change isTerminal call.cancel.state
    rw [cancel_state call]
    exact Or.inl rfl

theorem reconstructedSlotCount_bounded_by_max_concurrent
    {callIds : Finset Nat}
    {row : Nat → InferenceCall}
    {scheduler : SchedulerState}
    (h_reconstruct : ReconstructsSchedulerRunning callIds row scheduler)
    (h_capacity : SchedulerState.capacityInvariant scheduler)
    (bid : BackendId) :
    reconstructedSlotCount callIds row bid ≤ (scheduler.backends bid).max_concurrent := by
  rw [← h_reconstruct bid]
  exact h_capacity bid

end InferenceCall
