import Proofs.InferenceCall.SlotAccounting
import Mathlib.Algebra.Order.BigOperators.Group.Finset

open scoped BigOperators

/-!
# In-memory admission controller bookkeeping (#1001)

`Proofs/InferenceCall/SlotAccounting.lean` reasons about *persisted* running
rows. The runtime additionally keeps two in-memory counters on each
`BackendAdmissionController` (`crates/gents/src/admission/controller.rs`) that
the persisted model abstracts over:

- the **queue-waiter** counter bounding `max_queue_depth`, and
- the **in-flight** counter that drain detection (`is_drained`) reads before
  `AdmissionRegistry::reconcile` installs a replacement controller.

Issue #1001 found two defects that live exactly in that gap:

1. a fallible durable write between the waiter increment and the guard that
   decrements it leaked queue capacity on persist failure, and
2. in-flight admissions were counted only *after* semaphore acquisition, so a
   permit-holder in the acquire→count window was invisible to `is_drained`,
   letting reconcile install a fresh full-capacity controller while an old
   permit was live.

This module models one call's path through `acquire` as a phase machine and
assigns each phase its waiter / in-flight / semaphore-permit contribution.
The load-bearing facts are:

- `permit_implies_in_flight` / `drained_no_outstanding_permits`: with
  in-flight counted from `enteredAcquire` (acquisition *intent*), a drained
  controller holds no permits, so `replacement_preserves_capacity` keeps the
  per-backend bound after a controller swap.
- `late_admission_counting_unsound`: counting in-flight only at `admitted`
  (the pre-fix Rust behavior) violates that invariant — the model rejects the
  buggy counting rather than merely not mentioning it.
- `persist_error_releases_waiter` and `terminal_phase_releases_bookkeeping`:
  every terminal outcome — including the queued-persist failure path —
  contributes zero to both counters (the S9 analog for in-memory bookkeeping).

The model's `release` action drops the permit and in-flight contributions
atomically. Rust refines that edge by returning the semaphore permit
*before* decrementing `in_flight` on every release path — the release can
synchronously install a replacement controller, so the reverse order would
let a drained controller briefly hold an outstanding permit.

No contract JSON is emitted for this module; the Rust fence is
`crates/gents/src/admission/tests.rs`
(`queued_persist_failure_releases_queue_capacity`,
`assigned_permit_is_visible_to_drain_detection`,
`drained_signal_implies_permit_returned`), which drives the real
controller through these paths and asserts the modeled contributions.
-/

namespace InferenceCall
namespace ControllerBookkeeping

/-- Phases of a single call's path through `BackendAdmissionController::acquire`. -/
inductive AdmissionPhase where
  /-- Inside `acquire`, before any semaphore outcome. Counted in flight. -/
  | enteredAcquire
  /-- Waiter counted; the durable queued row has not been written yet. -/
  | queuedUnpersisted
  /-- Waiter counted; queued row durable; parked on the semaphore. -/
  | queuedWaiting
  /-- A semaphore permit is issued to this call, including the window where the
      permit is assigned to a parked waiter that has not resumed yet. -/
  | permitIssued
  /-- The permit was transferred into a live `AdmissionPermit`. -/
  | admitted
  /-- Terminal without admission: closed backend, full queue, or persist error. -/
  | rejected
  /-- Terminal after admission: the `AdmissionPermit` released capacity. -/
  | released
  deriving DecidableEq, Repr

namespace AdmissionPhase

def isTerminalPhase : AdmissionPhase → Prop
  | .rejected => True
  | .released => True
  | _ => False

instance : DecidablePred isTerminalPhase := by
  intro p
  cases p <;> simp [isTerminalPhase] <;> infer_instance

/-- Units held against `max_queue_depth` (the Rust `waiters` counter). -/
def waiterContribution : AdmissionPhase → Nat
  | .queuedUnpersisted => 1
  | .queuedWaiting => 1
  | _ => 0

/-- Units visible to drain detection (the Rust `in_flight` counter read by
    `is_drained`). Counted from acquisition intent, not from admission. -/
def inFlightContribution : AdmissionPhase → Nat
  | .rejected => 0
  | .released => 0
  | _ => 1

/-- Outstanding semaphore permits attributable to this call. -/
def permitContribution : AdmissionPhase → Nat
  | .permitIssued => 1
  | .admitted => 1
  | _ => 0

/-- The pre-fix Rust counting: an admission became visible to drain detection
    only after the permit was already transferred. Kept only as the witness
    vocabulary for `late_admission_counting_unsound`. -/
def lateInFlightContribution : AdmissionPhase → Nat
  | .admitted => 1
  | _ => 0

end AdmissionPhase

open AdmissionPhase

/-- Transition vocabulary mirroring the branches of
    `BackendAdmissionController::acquire`. -/
inductive Action where
  /-- The controller observed `closed` before acquiring. -/
  | rejectClosed
  /-- `try_acquire_owned` succeeded immediately. -/
  | tryAcquireIssued
  /-- The waiter CAS succeeded under `max_queue_depth`. -/
  | enterQueue
  /-- The waiter CAS refused and the post-refusal retry found no permit. -/
  | rejectQueueFull
  /-- The durable queued row was written. -/
  | persistQueuedOk
  /-- The durable queued write failed (issue #1001 defect 1 path). -/
  | persistQueuedErr
  /-- The semaphore granted the parked waiter a permit. -/
  | queueAcquireIssued
  /-- The semaphore closed while the waiter was parked. -/
  | queueRejectClosed
  /-- The permit was transferred into a live `AdmissionPermit`. -/
  | admit
  /-- The running write failed and the permit was returned. -/
  | rejectRunningPersistErr
  /-- The `AdmissionPermit` released capacity. -/
  | release
  deriving DecidableEq, Repr

def step? : AdmissionPhase → Action → Option AdmissionPhase
  | .enteredAcquire, .rejectClosed => some .rejected
  | .enteredAcquire, .tryAcquireIssued => some .permitIssued
  | .enteredAcquire, .enterQueue => some .queuedUnpersisted
  | .enteredAcquire, .rejectQueueFull => some .rejected
  | .queuedUnpersisted, .persistQueuedOk => some .queuedWaiting
  | .queuedUnpersisted, .persistQueuedErr => some .rejected
  | .queuedWaiting, .queueAcquireIssued => some .permitIssued
  | .queuedWaiting, .queueRejectClosed => some .rejected
  | .permitIssued, .admit => some .admitted
  | .permitIssued, .rejectRunningPersistErr => some .rejected
  | .admitted, .release => some .released
  | _, _ => none

def replay? : AdmissionPhase → List Action → Option AdmissionPhase
  | p, [] => some p
  | p, action :: rest =>
      match step? p action with
      | some p' => replay? p' rest
      | none => none

/-- Every phase that holds a semaphore permit is visible to drain detection.
    This is the invariant the pre-fix Rust counting violated. -/
theorem permit_implies_in_flight (p : AdmissionPhase) :
    permitContribution p ≤ inFlightContribution p := by
  cases p <;> simp [permitContribution, inFlightContribution]

/-- Every counted queue waiter is also visible to drain detection. -/
theorem waiter_implies_in_flight (p : AdmissionPhase) :
    waiterContribution p ≤ inFlightContribution p := by
  cases p <;> simp [waiterContribution, inFlightContribution]

/-- Counting in-flight admissions only at `admitted` — incrementing after
    semaphore acquisition, as the pre-#1001 Rust did — is unsound:
    `permitIssued` holds a permit invisibly. -/
theorem late_admission_counting_unsound :
    ¬ ∀ p : AdmissionPhase, permitContribution p ≤ lateInFlightContribution p := by
  intro h
  have := h .permitIssued
  simp [permitContribution, lateInFlightContribution] at this

/-- Terminal admission outcomes hold no bookkeeping: no waiter unit, no
    in-flight unit, no permit. The in-memory S9 analog. -/
theorem terminal_phase_releases_bookkeeping
    {p : AdmissionPhase} (h_terminal : isTerminalPhase p) :
    waiterContribution p = 0 ∧ inFlightContribution p = 0 ∧ permitContribution p = 0 := by
  cases p <;>
    simp [isTerminalPhase] at h_terminal <;>
    simp [waiterContribution, inFlightContribution, permitContribution]

/-- The queued-persist failure path is terminal and releases the waiter unit.
    Issue #1001 defect 1: the pre-fix Rust left the waiter counted forever on
    this path, permanently shrinking queue capacity toward `QueueFull`. -/
theorem persist_error_releases_waiter
    {p q : AdmissionPhase}
    (h_step : step? p .persistQueuedErr = some q) :
    isTerminalPhase q ∧ waiterContribution q = 0 ∧ inFlightContribution q = 0 := by
  cases p <;> simp [step?] at h_step
  simp [← h_step, isTerminalPhase, waiterContribution, inFlightContribution]

/-- Steps never step out of a terminal phase. -/
theorem terminal_phase_no_successor
    {p : AdmissionPhase} (h_terminal : isTerminalPhase p) (action : Action) :
    step? p action = none := by
  cases p <;> cases action <;>
    first
      | rfl
      | simp [isTerminalPhase] at h_terminal

/-- Every non-terminal phase has a finite legal path to a terminal phase
    (tier-1 reachability: no admission phase is modeled as stuck). -/
theorem nonterminal_reaches_terminal
    (p : AdmissionPhase) (h_live : ¬ isTerminalPhase p) :
    ∃ (actions : List Action) (q : AdmissionPhase),
      replay? p actions = some q ∧ isTerminalPhase q := by
  cases p with
  | enteredAcquire =>
      exact ⟨[.rejectClosed], .rejected, by simp [replay?, step?], by simp [isTerminalPhase]⟩
  | queuedUnpersisted =>
      exact ⟨[.persistQueuedErr], .rejected, by simp [replay?, step?], by simp [isTerminalPhase]⟩
  | queuedWaiting =>
      exact ⟨[.queueRejectClosed], .rejected, by simp [replay?, step?], by simp [isTerminalPhase]⟩
  | permitIssued =>
      exact ⟨[.admit, .release], .released, by simp [replay?, step?], by simp [isTerminalPhase]⟩
  | admitted =>
      exact ⟨[.release], .released, by simp [replay?, step?], by simp [isTerminalPhase]⟩
  | rejected => exact absurd (by simp [isTerminalPhase]) h_live
  | released => exact absurd (by simp [isTerminalPhase]) h_live

/-- Aggregate in-flight count over a controller's calls (what `is_drained`
    reads, in the style of `reconstructedSlotCount`). -/
def controllerInFlight (callIds : Finset Nat) (phase : Nat → AdmissionPhase) : Nat :=
  ∑ callId ∈ callIds, inFlightContribution (phase callId)

/-- Aggregate outstanding semaphore permits over a controller's calls. -/
def controllerPermits (callIds : Finset Nat) (phase : Nat → AdmissionPhase) : Nat :=
  ∑ callId ∈ callIds, permitContribution (phase callId)

/-- Drain detection: no call is visible in flight. -/
def drained (callIds : Finset Nat) (phase : Nat → AdmissionPhase) : Prop :=
  controllerInFlight callIds phase = 0

/-- A drained controller holds no outstanding semaphore permits — including
    permits assigned to parked waiters that have not resumed. Issue #1001
    defect 3: with post-acquisition counting this is false, and reconcile
    could replace a controller that still had a live permit. -/
theorem drained_no_outstanding_permits
    {callIds : Finset Nat} {phase : Nat → AdmissionPhase}
    (h_drained : drained callIds phase) :
    controllerPermits callIds phase = 0 := by
  have h_le : controllerPermits callIds phase ≤ controllerInFlight callIds phase :=
    Finset.sum_le_sum fun callId _ => permit_implies_in_flight (phase callId)
  have h_zero : controllerInFlight callIds phase = 0 := h_drained
  omega

/-- Installing a replacement controller after drain preserves the backend
    capacity bound: the old controller contributes zero permits, so the
    combined outstanding permits stay within `max_concurrent`. -/
theorem replacement_preserves_capacity
    {oldIds newIds : Finset Nat}
    {oldPhase newPhase : Nat → AdmissionPhase}
    {maxConcurrent : Nat}
    (h_drained : drained oldIds oldPhase)
    (h_new : controllerPermits newIds newPhase ≤ maxConcurrent) :
    controllerPermits oldIds oldPhase + controllerPermits newIds newPhase ≤ maxConcurrent := by
  rw [drained_no_outstanding_permits h_drained]
  omega

end ControllerBookkeeping
end InferenceCall
