import Proofs.Recovery.Sweeps.RequestResponse
import Proofs.Recovery.Sweeps.Inference

/-!
# Startup recovery ordering: requests before inference calls (#1001)

`inferenceCallRecoverySweep` is pinned `cadence := .startup`, but its Rust
implementation (`InferenceCall::recover_all`) is *parent-gated*: it skips any
queued/running call whose linked `AgentRequest` is not interrupted or terminal.
That gate is deliberate — a live parent means the call may still be owned by a
running loop — but it makes the sweep's convergence depend on the request sweep
having run first.

After a crash, the parents of every stale call are exactly the requests stuck
in `claimed`/`processing`. The pre-#1001 startup ran the inference sweep
*before* `RequestLifecycle::recover_all`, so the gate skipped every
crash-orphaned call, no later sweep re-ran it, and orphaned `running` rows
survived a full session — understating backend capacity
(`Proofs/InferenceCall/SlotAccounting.lean` counts running rows as held slots)
until a second restart.

This module models the gate and pins the ordering contract:

- `inference_first_skips_crash_orphan` / `inference_before_request_leaves_call_live`
  state the defect: with the sweeps in the wrong order, a crash-orphaned call
  is untouched and a running orphan still holds its slot after startup.
- `request_before_inference_converges` is the contract the runtime implements:
  running the request sweep first terminalizes the parent (its durable outcome
  exists because response recovery writes one for every stuck request), after
  which the gated inference sweep terminalizes the call and its slot
  contribution reaches zero in the same startup pass.

The Rust fence is
`tests/conformance/recovery_sweeps.rs::startup_recovery_order_terminalizes_crash_orphaned_calls`,
which drives the real ordered startup sweep (`gents::startup_recovery`) over a
seeded crash state.
-/

namespace Recovery

/-- A crash-orphaned pair: a stuck parent request and the live inference call
    linked to it by `request_id`. -/
structure OrphanedCallState where
  parent : RequestRecoveryRow
  call : InferenceCallRecoveryRow
  deriving Repr

/-- The parent gate of `InferenceCall::recover_all` (`recovery_outcome` in
    `crates/gents/src/admission/recovery.rs`): an interrupted parent cancels,
    a terminal parent terminalizes by call state, a live parent is skipped. -/
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

/-- The startup inference-call sweep as the runtime actually runs it: the
    registered recover function behind the parent gate. -/
def gatedInferenceSweep (s : OrphanedCallState) : OrphanedCallState :=
  match gatedInferenceCause s.parent.request.state s.call.call.state with
  | some cause => { s with call := inferenceCallRecover { s.call with cause := cause } }
  | none => s

/-- The startup request sweep applied to the parent row. -/
def requestSweep (s : OrphanedCallState) : OrphanedCallState :=
  if requestRecoveryStale s.parent then { s with parent := requestRecover s.parent } else s

/-- The crash shape: the parent is stuck `claimed`/`processing` with a durable
    outcome (response recovery guarantees one exists for every stuck request),
    and the linked call is still `queued`/`running`. -/
def crashOrphaned (s : OrphanedCallState) : Prop :=
  (s.parent.request.state = .claimed ∨ s.parent.request.state = .processing) ∧
    s.parent.durableOutcome ≠ .absent ∧
    (s.call.call.state = .queued ∨ s.call.call.state = .running)

/-- The defect gate shape: with a crash-stuck (non-terminal) parent, the gated
    inference sweep is a no-op on the call. -/
theorem inference_first_skips_crash_orphan
    {s : OrphanedCallState} (h_crash : crashOrphaned s) :
    (gatedInferenceSweep s).call = s.call := by
  rcases h_crash with ⟨h_parent, _h_outcome, _h_call⟩
  cases h_parent with
  | inl h_claimed => simp [gatedInferenceSweep, gatedInferenceCause, h_claimed]
  | inr h_processing => simp [gatedInferenceSweep, gatedInferenceCause, h_processing]

/-- Issue #1001 defect 2, stated end to end: running the inference sweep
    before the request sweep leaves a crash-orphaned call in its live state —
    a running orphan still holds its backend slot after startup recovery, and
    no later sweep runs at `.startup` cadence again. -/
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

/-- The ordering contract the runtime implements: request sweep first, then
    the gated inference sweep. One startup pass terminalizes the parent,
    releases its admission, terminalizes the orphaned call, and drops its
    slot contribution to zero. -/
theorem request_before_inference_converges
    {s : OrphanedCallState} (h_crash : crashOrphaned s) :
    let s' := gatedInferenceSweep (requestSweep s)
    isTerminal s'.parent.request.state ∧
      s'.parent.request.admission = .released ∧
      isTerminal s'.call.call.state ∧
      ∀ bid : BackendId, s'.call.call.slotContribution bid = 0 := by
  rcases h_crash with ⟨h_parent, h_outcome, h_call⟩
  have h_stale : requestRecoveryStale s.parent := ⟨h_parent, h_outcome⟩
  have h_sweep : requestSweep s = { s with parent := requestRecover s.parent } := by
    simp [requestSweep, h_stale]
  rw [h_sweep]
  cases h_outcome_value : s.parent.durableOutcome with
  | absent => exact absurd h_outcome_value h_outcome
  | completed =>
      cases h_call with
      | inl h_queued =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_outcome_value, h_queued, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]
      | inr h_running =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_outcome_value, h_running, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]
  | failed =>
      cases h_call with
      | inl h_queued =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_outcome_value, h_queued, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]
      | inr h_running =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_outcome_value, h_running, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]
  | interrupted =>
      cases h_call with
      | inl h_queued =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_outcome_value, h_queued, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]
      | inr h_running =>
          refine ⟨?_, ?_, ?_, ?_⟩ <;>
            simp [gatedInferenceSweep, gatedInferenceCause, requestRecover,
              recoveredRequestState, h_outcome_value, h_running, inferenceCallRecover,
              HasTerminal.isTerminal, RequestState.instHasTerminal,
              InferenceCallState.instHasTerminal, InferenceCall.slotContribution,
              InferenceCall.holdsBackendSlot, InferenceCallState.holdsBackendSlot]

end Recovery
