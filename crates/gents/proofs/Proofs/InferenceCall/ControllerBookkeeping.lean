import Proofs.InferenceCall.SlotAccounting

/-!
Phases of one call's path through the in-memory admission controller
(`crates/gents/src/admission/controller.rs`): each phase's waiter-unit and
semaphore-permit contribution to `max_queue_depth` and the pool permits.
`persist_error_releases_waiter` and `terminal_phase_releases_bookkeeping`
state that every terminal outcome, including the queued-persist failure path
(issue #1001), contributes zero to both. Every controller incarnation of a
pool shares its permits (`InferenceCall.Registry`), so no controller-level
drain count exists. The Rust fence is
`crates/gents/src/admission/tests.rs::queued_persist_failure_releases_queue_capacity`.
-/

namespace InferenceCall
namespace ControllerBookkeeping

inductive AdmissionPhase where
  | enteredAcquire
  | queuedUnpersisted
  | queuedWaiting
  | permitIssued
  | admitted
  | rejected
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

def waiterContribution : AdmissionPhase → Nat
  | .queuedUnpersisted => 1
  | .queuedWaiting => 1
  | _ => 0

def permitContribution : AdmissionPhase → Nat
  | .permitIssued => 1
  | .admitted => 1
  | _ => 0

end AdmissionPhase

open AdmissionPhase

inductive Action where
  | rejectClosed
  | tryAcquireIssued
  | enterQueue
  | rejectQueueFull
  | persistQueuedOk
  | persistQueuedErr
  | queueAcquireIssued
  | queueRejectClosed
  | admit
  | rejectRunningPersistErr
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

theorem terminal_phase_releases_bookkeeping
    {p : AdmissionPhase} (h_terminal : isTerminalPhase p) :
    waiterContribution p = 0 ∧ permitContribution p = 0 := by
  cases p <;>
    simp [isTerminalPhase] at h_terminal <;>
    simp [waiterContribution, permitContribution]

theorem persist_error_releases_waiter
    {p q : AdmissionPhase}
    (h_step : step? p .persistQueuedErr = some q) :
    isTerminalPhase q ∧ waiterContribution q = 0 ∧ permitContribution q = 0 := by
  cases p <;> simp [step?] at h_step
  simp [← h_step, isTerminalPhase, waiterContribution, permitContribution]

theorem terminal_phase_no_successor
    {p : AdmissionPhase} (h_terminal : isTerminalPhase p) (action : Action) :
    step? p action = none := by
  cases p <;> cases action <;>
    first
      | rfl
      | simp [isTerminalPhase] at h_terminal

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
