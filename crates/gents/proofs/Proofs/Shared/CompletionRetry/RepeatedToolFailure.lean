import Proofs.CompletionRetry.InvalidToolProgress

/-! The guard folds the execution's dispatch record and is execution-local; it is
not persisted and owns no request lifecycle. Either ending asks the existing
stream error path and execution lease owner to terminalize. Dispatch is
serialized, so eligibility is decided before each call. Only ordinary failures
with a stable owner-derived error identity are counted; the runtime identifies a
call by tool name and argument text, and the native adapter excludes volatile
execution metadata from the error identity. Other outcomes restart the streak. -/

namespace CompletionRetry.RepeatedToolFailure

open CompletionRetry

/-- Identical failing dispatches the loop performs before it stops running
that call. Three lets a model observe a deterministic failure more than once
before the loop intervenes. -/
def failureLimit : Nat := 3

/-- Undispatched identical repeats before the execution fails. The first
repeat returns a notice as the call's result so the model can change course;
the next one ends the execution. -/
def suppressionLimit : Nat := 2

/-- One accepted call and the outcome the loop observes if it is admitted and
dispatched. `.skipped` is a call the persistence hook answers itself. `error`
is the stable identity of an ordinary failure, when one can be derived; it is
supplied by the owner of the result, so only the tool that renders an output
format decides which of its fields are volatile. -/
structure Event where
  call : Nat
  outcome : InvalidToolProgress.Outcome
  error : Option Nat := none
  deriving DecidableEq, Repr

def countedError (e : Event) : Option Nat :=
  if e.outcome = .ordinaryFailure then e.error else none

theorem charged_outcomes_do_not_count (e : Event)
    (h : InvalidToolProgress.invalid e.outcome = true) : countedError e = none := by
  cases e with
  | mk call outcome error =>
      cases outcome <;> simp_all [countedError, InvalidToolProgress.invalid]

structure State where
  streak : Option (Nat × Nat) := none
  failures : Nat := 0
  suppressed : Nat := 0
  deriving DecidableEq, Repr

inductive Decision
  | dispatch
  | suppress
  | stop
  deriving DecidableEq, Repr

def repeatsStreak (s : State) (call : Nat) : Bool :=
  match s.streak with
  | some (streakCall, _) => streakCall == call && failureLimit ≤ s.failures
  | none => false

def decide (s : State) (call : Nat) : Decision :=
  if repeatsStreak s call then
    if s.suppressed + 1 < suppressionLimit then .suppress else .stop
  else .dispatch

def recordDispatched (s : State) (call : Nat) : Option Nat → State
  | some error =>
      if s.streak == some (call, error) then { s with failures := s.failures + 1 }
      else { streak := some (call, error), failures := 1, suppressed := 0 }
  | none => {}

def recordSuppressed (s : State) : State := { s with suppressed := s.suppressed + 1 }

def Bounded (s : State) : Prop :=
  s.failures ≤ failureLimit ∧ s.suppressed < suppressionLimit

theorem initial_bounded : Bounded {} := by
  simp [Bounded, failureLimit, suppressionLimit]

theorem dispatched_bounded (s : State) (call : Nat) (error : Option Nat)
    (h : Bounded s) (hd : decide s call = .dispatch) :
    Bounded (recordDispatched s call error) := by
  obtain ⟨hf, hs⟩ := h
  have hr : repeatsStreak s call = false := by
    unfold decide at hd
    by_cases hr : repeatsStreak s call = true
    · simp only [hr, if_true] at hd
      split at hd <;> simp at hd
    · simpa using hr
  cases error with
  | none => simp [recordDispatched, Bounded, failureLimit, suppressionLimit]
  | some error =>
      by_cases heq : (s.streak == some (call, error)) = true
      · have hstreak : s.streak = some (call, error) := by simpa using heq
        have hlow : s.failures < failureLimit := by
          simp [repeatsStreak, hstreak] at hr
          exact hr
        simp only [recordDispatched, heq, if_true]
        exact ⟨hlow, hs⟩
      · simp only [recordDispatched, heq, Bool.false_eq_true, if_false]
        simp [Bounded, failureLimit, suppressionLimit]

theorem suppressed_bounded (s : State) (call : Nat)
    (h : Bounded s) (hd : decide s call = .suppress) :
    Bounded (recordSuppressed s) := by
  obtain ⟨hf, _⟩ := h
  unfold decide at hd
  by_cases hr : repeatsStreak s call = true
  · by_cases hlt : s.suppressed + 1 < suppressionLimit
    · exact ⟨hf, hlt⟩
    · simp [hr, hlt] at hd
  · simp [hr] at hd

theorem limit_blocks_identical (s : State) (call error : Nat)
    (hstreak : s.streak = some (call, error)) (hlimit : failureLimit ≤ s.failures) :
    decide s call ≠ .dispatch := by
  unfold decide
  simp [repeatsStreak, hstreak, hlimit]
  split <;> simp

theorem different_call_dispatches (s : State) (call streakCall error : Nat)
    (hstreak : s.streak = some (streakCall, error)) (hne : streakCall ≠ call) :
    decide s call = .dispatch := by
  simp [decide, repeatsStreak, hstreak, hne]

structure Composed where
  invalid : InvalidToolProgress.State := {}
  repeats : State := {}
  deriving DecidableEq, Repr

inductive Action
  | dispatch
  | suppress
  | skip
  | stop
  deriving DecidableEq, Repr

inductive Ending
  | repeatedFailure
  | invalidExhausted
  deriving DecidableEq, Repr

inductive Step
  | continue (action : Action) (next : Composed)
  | ended (action : Action) (ending : Ending) (invalid : InvalidToolProgress.State)
  deriving DecidableEq, Repr

def admit (s : Composed) (e : Event) (suppressed : Bool) : Step :=
  if e.outcome = .skipped then .continue .skip { s with repeats := {} }
  else
    let observed := if suppressed then .policyDenied else e.outcome
    let invalid := InvalidToolProgress.recordDurable s.invalid observed
    let repeats :=
      if suppressed then recordSuppressed s.repeats
      else recordDispatched s.repeats e.call (countedError e)
    let action := if suppressed then Action.suppress else Action.dispatch
    if InvalidToolProgress.exhausted invalid then .ended action .invalidExhausted invalid
    else .continue action ⟨invalid, repeats⟩

def step (s : Composed) (e : Event) : Step :=
  match decide s.repeats e.call with
  | .stop => .ended .stop .repeatedFailure s.invalid
  | .dispatch => admit s e false
  | .suppress => admit s e true

def run (s : Composed) : List Event → List Action × Option Ending
  | [] => ([], none)
  | e :: rest =>
      match step s e with
      | .ended action ending _ => ([action], some ending)
      | .continue action next =>
          let (actions, ending) := run next rest
          (action :: actions, ending)

def invalidUsed (s : Composed) : List Event → Nat
  | [] => s.invalid.invalidUsed
  | e :: rest =>
      match step s e with
      | .ended _ _ invalid => invalid.invalidUsed
      | .continue _ next => invalidUsed next rest

def actionName : Action → String
  | .dispatch => "dispatch"
  | .suppress => "suppress"
  | .skip => "skip"
  | .stop => "stop"

def endingName : Ending → String
  | .repeatedFailure => "repeatedFailure"
  | .invalidExhausted => "invalidExhausted"

theorem step_bounded (s : Composed) (e : Event) (h : Bounded s.repeats) :
    ∀ action next, step s e = .continue action next → Bounded next.repeats := by
  intro action next hstep
  unfold step at hstep
  cases hd : decide s.repeats e.call with
  | stop => simp [hd] at hstep
  | dispatch =>
      simp only [hd, admit] at hstep
      by_cases hs : e.outcome = .skipped
      · simp [hs] at hstep
        rcases hstep with ⟨-, rfl⟩
        exact initial_bounded
      · by_cases hx : InvalidToolProgress.exhausted
            (InvalidToolProgress.recordDurable s.invalid e.outcome) = true
        · simp [hs, hx] at hstep
        · simp [hs, hx] at hstep
          rcases hstep with ⟨-, rfl⟩
          exact dispatched_bounded _ _ _ h hd
  | suppress =>
      simp only [hd, admit] at hstep
      by_cases hs : e.outcome = .skipped
      · simp [hs] at hstep
        rcases hstep with ⟨-, rfl⟩
        exact initial_bounded
      · by_cases hx : InvalidToolProgress.exhausted
            (InvalidToolProgress.recordDurable s.invalid .policyDenied) = true
        · simp [hs, hx] at hstep
        · simp [hs, hx] at hstep
          rcases hstep with ⟨-, rfl⟩
          exact suppressed_bounded _ _ h hd

theorem skipped_resets (s : Composed) (e : Event) (hskip : e.outcome = .skipped)
    (hd : decide s.repeats e.call ≠ .stop) :
    step s e = .continue .skip { s with repeats := {} } := by
  unfold step
  cases h : decide s.repeats e.call with
  | stop => exact absurd h hd
  | dispatch => simp [admit, hskip]
  | suppress => simp [admit, hskip]

theorem suppression_is_charged (s : Composed) (e : Event)
    (hd : decide s.repeats e.call = .suppress) (hskip : e.outcome ≠ .skipped)
    (action : Action) (next : Composed) (hstep : step s e = .continue action next) :
    action = .suppress ∧
      next.invalid = InvalidToolProgress.recordDurable s.invalid .policyDenied := by
  simp only [step, hd, admit] at hstep
  by_cases hx : InvalidToolProgress.exhausted
      (InvalidToolProgress.recordDurable s.invalid .policyDenied) = true
  · simp [hskip, hx] at hstep
  · simp [hskip, hx] at hstep
    rcases hstep with ⟨rfl, rfl⟩
    exact ⟨rfl, rfl⟩

private def fail (call error : Nat) : Event := ⟨call, .ordinaryFailure, some error⟩

theorem identical_failures_stop_on_fifth (rest : List Event) :
    run {} (List.replicate 5 (fail 0 0) ++ rest) =
      ([.dispatch, .dispatch, .dispatch, .suppress, .stop], some .repeatedFailure) := by
  rfl

theorem suppression_can_exhaust_invalid_allowance :
    run { invalid := ⟨7⟩ } (List.replicate 4 (fail 0 0)) =
      ([.dispatch, .dispatch, .dispatch, .suppress], some .invalidExhausted) := by
  rfl

end CompletionRetry.RepeatedToolFailure
