import Proofs.CompletionRetry.InvalidToolProgress

/-! Repetition guard of the owned completion loop (#1734), composed with the
invalid-tool allowance.

A model can reissue one call whose arguments the tool rejects until the turn
cap: the observed failure was 234 dispatches of one call returning the same
tool error. Those were ordinary tool failures, which `InvalidToolProgress`
deliberately does not charge.

The guard is a fold over the execution's own dispatch record, over the loop's
real outcome vocabulary (`InvalidToolProgress.Outcome`). It is execution-local,
is not persisted, and owns no request lifecycle; either ending asks the
existing stream error path and execution lease owner to terminalize. Dispatch
is serialized, so a provider turn with several calls decides before each.

A call is identified by its tool name and argument text. Only an ordinary
failure whose error identity the native adapter can derive is counted; the
identity excludes volatile execution metadata. Everything else restarts the
streak: success, a call with other arguments or another error, an outcome the
invalid allowance charges, a call the persistence hook handles itself (unless
the guard already decided to stop it, which happens before the hook runs), and
an ordinary failure without a derivable identity. Interleaved repeats are left to
the turn cap and other budgets. -/
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

/-- Outcomes the invalid allowance charges never count here. -/
theorem charged_outcomes_do_not_count (e : Event)
    (h : InvalidToolProgress.invalid e.outcome = true) : countedError e = none := by
  cases e with
  | mk call outcome error =>
      cases outcome <;> simp_all [countedError, InvalidToolProgress.invalid]

structure State where
  /-- The call and error of the current streak of identical failures. -/
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

/-- After `failureLimit` identical failures the same call is not dispatched. -/
theorem limit_blocks_identical (s : State) (call error : Nat)
    (hstreak : s.streak = some (call, error)) (hlimit : failureLimit ≤ s.failures) :
    decide s call ≠ .dispatch := by
  unfold decide
  simp [repeatsStreak, hstreak, hlimit]
  split <;> simp

/-- A call with other arguments is always dispatched. -/
theorem different_call_dispatches (s : State) (call streakCall error : Nat)
    (hstreak : s.streak = some (streakCall, error)) (hne : streakCall ≠ call) :
    decide s call = .dispatch := by
  simp [decide, repeatsStreak, hstreak, hne]

/-! ### Composition with `InvalidToolProgress`

The suppression notice is a policy denial, so the invalid allowance charges
it like any other refused call; an execution whose allowance is nearly spent
can therefore end at a suppression with `invalidExhausted`. A hook-handled call
is not charged and restarts the streak. -/

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

/-- The admitted call's outcome is durably observed, then charged. -/
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

/-- A stop is decided before the hook admits the call, so the accepted call is
never admitted and its intent is settled by the terminal owner. -/
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

/-- Invalid allowance spent when the trace ends or is exhausted. -/
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

/-- The streak and suppression bounds hold across every composed step. -/
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

/-- A call the hook answers itself restarts the streak unless it is the
stopped repeat. -/
theorem skipped_resets (s : Composed) (e : Event) (hskip : e.outcome = .skipped)
    (hd : decide s.repeats e.call ≠ .stop) :
    step s e = .continue .skip { s with repeats := {} } := by
  unfold step
  cases h : decide s.repeats e.call with
  | stop => exact absurd h hd
  | dispatch => simp [admit, hskip]
  | suppress => simp [admit, hskip]

/-- The suppression spends one unit of the invalid allowance. -/
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

/-- The observed failure: an identical failing call runs three times, is
answered once with a notice, and stops the execution on the fifth call. -/
theorem identical_failures_stop_on_fifth (rest : List Event) :
    run {} (List.replicate 5 (fail 0 0) ++ rest) =
      ([.dispatch, .dispatch, .dispatch, .suppress, .stop], some .repeatedFailure) := by
  rfl

/-- With seven invalid outcomes spent, the charged suppression exhausts the
allowance and ends the execution before the repeat can stop it. -/
theorem suppression_can_exhaust_invalid_allowance :
    run { invalid := ⟨7⟩ } (List.replicate 4 (fail 0 0)) =
      ([.dispatch, .dispatch, .dispatch, .suppress], some .invalidExhausted) := by
  rfl

end CompletionRetry.RepeatedToolFailure
