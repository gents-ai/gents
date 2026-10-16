import Proofs.Recovery.Sweeps.Requests
import Proofs.Recovery.Sweeps.Inference

/-!
# Startup recovery ordering: requests before inference calls (#1001)

`inferenceCallRecoverySweep` is parent-gated (`recovery_outcome` in
`crates/gents/src/admission/recovery.rs`): live parents are skipped, so its
convergence depends on the request sweep running first. Native fence:
`tests/conformance/recovery_sweeps.rs::startup_recovery_order_terminalizes_crash_orphaned_calls`.
-/

namespace Recovery

structure OrphanedCallState where
  parent : RequestRecoveryRow
  call : InferenceCallRecoveryRow
  deriving Repr

def gatedInferenceCause :
    RequestState → InferenceCallState → Option InferenceRecoveryCause
  | .interrupted, .queued => some .interruptedParent
  | .interrupted, .running => some .interruptedParent
  | .completed, .queued => some .staleQueued
  | .completed, .running => some .staleRunning
  | .failed, .queued => some .staleQueued
  | .failed, .running => some .staleRunning
  | .superseded, .queued => some .staleQueued
  | .superseded, .running => some .staleRunning
  | .dead, .queued => some .staleQueued
  | .dead, .running => some .staleRunning
  | _, _ => none

def gatedInferenceSweep (s : OrphanedCallState) : OrphanedCallState :=
  match gatedInferenceCause s.parent.request.state s.call.call.state with
  | some cause => { s with call := inferenceCallRecover { s.call with cause := cause } }
  | none => s

def requestSweep (s : OrphanedCallState) : OrphanedCallState :=
  if requestRecoveryStale s.parent then { s with parent := requestRecover s.parent } else s

def crashOrphaned (s : OrphanedCallState) : Prop :=
  (s.parent.request.state = .claimed ∨ s.parent.request.state = .processing) ∧
    s.parent.leaseExpired = true ∧
    (s.call.call.state = .queued ∨ s.call.call.state = .running)

theorem inference_first_skips_crash_orphan
    {s : OrphanedCallState} (h_crash : crashOrphaned s) :
    (gatedInferenceSweep s).call = s.call := by
  rcases h_crash with ⟨h_parent, _h_expired, _h_call⟩
  cases h_parent with
  | inl h_claimed => simp [gatedInferenceSweep, gatedInferenceCause, h_claimed]
  | inr h_processing => simp [gatedInferenceSweep, gatedInferenceCause, h_processing]

theorem inference_before_request_leaves_call_live
    {s : OrphanedCallState} (h_crash : crashOrphaned s) :
    (requestSweep (gatedInferenceSweep s)).call.call.state = .queued ∨
      (requestSweep (gatedInferenceSweep s)).call.call.state = .running := by
  have h_call : (gatedInferenceSweep s).call = s.call :=
    inference_first_skips_crash_orphan h_crash
  have h_after : (requestSweep (gatedInferenceSweep s)).call = s.call := by
    unfold requestSweep
    split <;> simp [h_call]
  rw [h_after]
  exact h_crash.2.2

theorem request_before_inference_converges
    {s : OrphanedCallState} (h_crash : crashOrphaned s) :
    let s' := gatedInferenceSweep (requestSweep s)
    isTerminal s'.parent.request.state ∧
      s'.parent.request.admission = .released ∧
      isTerminal s'.call.call.state ∧
      ∀ bid : BackendId, s'.call.call.slotContribution bid = 0 := by
  rcases h_crash with ⟨h_parent, h_expired, h_call⟩
  have h_stale : requestRecoveryStale s.parent := ⟨h_parent, h_expired⟩
  have h_sweep : requestSweep s = { s with parent := requestRecover s.parent } := by
    simp [requestSweep, h_stale]
  rw [h_sweep]
  cases h_interrupt : s.parent.interruptRequested with
  | false =>
      cases h_call with
      | inl h_queued =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_interrupt, h_queued, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]
      | inr h_running =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_interrupt, h_running, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]
  | true =>
      cases h_call with
      | inl h_queued =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_interrupt, h_queued, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]
      | inr h_running =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_interrupt, h_running, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]

/-- Ordered sweep abstracts the already-proved generation-fenced lease-aware
request repair (defer until the observed execution deadline expires); it does
not create a second owner. -/
def leasedRequestSweep (now deadline : Nat) (s : OrphanedCallState) : OrphanedCallState :=
  if deadline < now then requestSweep s else s

def leasedRecoveryPass (now deadline : Nat) (s : OrphanedCallState) : OrphanedCallState :=
  gatedInferenceSweep (leasedRequestSweep now deadline s)

theorem live_lease_defers_request_and_inference
    {s : OrphanedCallState} (h_crash : crashOrphaned s)
    {now deadline : Nat} (h_live : now ≤ deadline) :
    leasedRecoveryPass now deadline s = s := by
  have h_not_expired : ¬ deadline < now := Nat.not_lt_of_ge h_live
  rcases h_crash with ⟨h_parent, _, _⟩
  cases h_parent with
  | inl h_claimed =>
      simp [leasedRecoveryPass, leasedRequestSweep, h_not_expired,
        gatedInferenceSweep, gatedInferenceCause, h_claimed]
  | inr h_processing =>
      simp [leasedRecoveryPass, leasedRequestSweep, h_not_expired,
        gatedInferenceSweep, gatedInferenceCause, h_processing]

theorem deadline_equality_preserves_request_and_inference
    {s : OrphanedCallState} (h_crash : crashOrphaned s) (deadline : Nat) :
    leasedRecoveryPass deadline deadline s = s :=
  live_lease_defers_request_and_inference h_crash (Nat.le_refl deadline)

/-- Finite two-pass trace conditional on expiry and a scheduled tick; no
scheduler fairness or wall-clock progress is assumed. Slot contribution is the
formal call-state projection, not the live process's counter. -/
theorem deferred_startup_then_expired_periodic_converges
    {s : OrphanedCallState} (h_crash : crashOrphaned s)
    {startupNow periodicNow deadline : Nat}
    (h_live : startupNow ≤ deadline) (h_expired : deadline < periodicNow) :
    let s' := leasedRecoveryPass periodicNow deadline
      (leasedRecoveryPass startupNow deadline s)
    isTerminal s'.parent.request.state ∧
      s'.parent.request.admission = .released ∧
      isTerminal s'.call.call.state ∧
      ∀ bid : BackendId, s'.call.call.slotContribution bid = 0 := by
  rw [live_lease_defers_request_and_inference h_crash h_live]
  simpa [leasedRecoveryPass, leasedRequestSweep, h_expired] using
    request_before_inference_converges h_crash

theorem inference_recovery_has_periodic_cadence :
    inferenceCallRecoverySweep.cadence = .periodic := rfl

end Recovery
