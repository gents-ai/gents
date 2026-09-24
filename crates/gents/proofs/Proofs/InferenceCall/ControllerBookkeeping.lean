import Proofs.InferenceCall.SlotAccounting

/-!
# In-memory admission controller bookkeeping (#1001)

`Proofs/InferenceCall/SlotAccounting.lean` reasons about *persisted* running
rows. The runtime additionally keeps in-memory bookkeeping on each backend's
admission capacity pool (`crates/gents/src/admission/controller.rs`) that the
persisted model abstracts over: the **queue-waiter** counter bounding
`max_queue_depth`, and the pool's semaphore permits.

Issue #1001 found that a fallible durable write between the waiter increment
and the guard that decrements it leaked queue capacity on persist failure.

This module models one call's path through `acquire` as a phase machine and
assigns each phase its waiter / semaphore-permit contribution.
`persist_error_releases_waiter` and `terminal_phase_releases_bookkeeping`
state that every terminal outcome, including the queued-persist failure path,
contributes zero to both. Every controller incarnation of a pool shares its
permits (`InferenceCall.Registry`), so no controller-level drain count exists.

No contract JSON is emitted for this module; the Rust fence is
`crates/gents/src/admission/tests.rs`
(`queued_persist_failure_releases_queue_capacity`), which drives the real
controller through this path and asserts the modeled contribution.
-/

namespace InferenceCall
namespace ControllerBookkeeping

/-- Phases of a single call's path through `BackendAdmissionController::acquire`. -/
inductive AdmissionPhase where
  /-- Inside `acquire`, before any semaphore outcome. -/
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
  /-- Terminal without admission: closed pool, full queue, or persist error. -/
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

/-- Outstanding semaphore permits attributable to this call. -/
def permitContribution : AdmissionPhase → Nat
  | .permitIssued => 1
  | .admitted => 1
  | _ => 0

end AdmissionPhase

open AdmissionPhase

/-- Transition vocabulary mirroring the branches of
    `BackendAdmissionController::acquire`. -/
inductive Action where
  /-- The pool was closed before a permit was issued. -/
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
  /-- The pool closed while the waiter was parked. -/
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

/-- Terminal admission outcomes hold no bookkeeping: no waiter unit and no
    permit. The in-memory S9 analog. -/
theorem terminal_phase_releases_bookkeeping
    {p : AdmissionPhase} (h_terminal : isTerminalPhase p) :
    waiterContribution p = 0 ∧ permitContribution p = 0 := by
  cases p <;>
    simp [isTerminalPhase] at h_terminal <;>
    simp [waiterContribution, permitContribution]

/-- The queued-persist failure path is terminal and releases the waiter unit.
    Issue #1001 defect 1: the pre-fix Rust left the waiter counted forever on
    this path, permanently shrinking queue capacity toward `QueueFull`. -/
theorem persist_error_releases_waiter
    {p q : AdmissionPhase}
    (h_step : step? p .persistQueuedErr = some q) :
    isTerminalPhase q ∧ waiterContribution q = 0 ∧ permitContribution q = 0 := by
  cases p <;> simp [step?] at h_step
  simp [← h_step, isTerminalPhase, waiterContribution, permitContribution]

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

end ControllerBookkeeping
end InferenceCall
