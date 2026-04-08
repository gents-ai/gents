import Proofs.Basic

/-!
# Layer 3: Persistence Lifecycle

Models the committed/uncommitted boundary for agent output.
Every piece of output (tokens, tool calls, compaction entries)
has a persistence state independent of the request lifecycle.
-/

/-- The 4 persistence states. -/
inductive PersistenceState where
  | uncommitted
  | committing
  | committed
  | lost
  deriving DecidableEq, Repr

namespace PersistenceState

instance : HasTerminal PersistenceState where
  isTerminal s := s = .committed ∨ s = .lost
  isTerminal_dec s :=
    match s with
    | .committed => isTrue (Or.inl rfl)
    | .lost => isTrue (Or.inr rfl)
    | .uncommitted => isFalse (by intro h; cases h with | inl h => exact absurd h (by decide) | inr h => exact absurd h (by decide))
    | .committing => isFalse (by intro h; cases h with | inl h => exact absurd h (by decide) | inr h => exact absurd h (by decide))

/-- Failure policy determines behavior on write failure. -/
inductive FailurePolicy where
  | failOpen
  | failClosed
  deriving DecidableEq, Repr

/-- Persistence transitions parameterized by failure policy. -/
inductive Transition (policy : FailurePolicy) :
    PersistenceState → PersistenceState → Prop where
  /-- Begin flushing buffered data. -/
  | flush :
      Transition policy .uncommitted .committing
  /-- Write succeeds — data is durable. -/
  | write_success :
      Transition policy .committing .committed
  /-- Write fails under FailClosed — return to uncommitted for retry. -/
  | write_fail_closed :
      policy = .failClosed →
      Transition policy .committing .uncommitted
  /-- Write fails under FailOpen — data acknowledged lost. -/
  | write_fail_open :
      policy = .failOpen →
      Transition policy .committing .lost
  /-- New data arrives while uncommitted — stays uncommitted. -/
  | accumulate :
      Transition policy .uncommitted .uncommitted

/-- Executable persistence actions mirroring `Transition`. -/
inductive Action where
  | flush
  | writeSuccess
  | writeFail
  | accumulate
  deriving DecidableEq, Repr

/-- Executable transition function for the persistence layer. -/
def step? (policy : FailurePolicy) (pre : PersistenceState) : Action → Option PersistenceState
  | .flush =>
      if pre = .uncommitted then some .committing else none
  | .writeSuccess =>
      if pre = .committing then some .committed else none
  | .writeFail =>
      if pre = .committing then
        some
          (match policy with
           | .failClosed => .uncommitted
           | .failOpen => .lost)
      else
        none
  | .accumulate =>
      if pre = .uncommitted then some .uncommitted else none

/-- A trace is a sequence of valid persistence transitions. -/
inductive Trace (policy : FailurePolicy) :
    PersistenceState → PersistenceState → Prop where
  | refl {s : PersistenceState} : Trace policy s s
  | step {s₁ s₂ s₃ : PersistenceState} :
      Transition policy s₁ s₂ → Trace policy s₂ s₃ → Trace policy s₁ s₃

/-- Replay a finite action list through the executable persistence semantics. -/
def replay? (policy : FailurePolicy) :
    PersistenceState → List Action → Option PersistenceState
  | s, [] => some s
  | s, action :: rest =>
      match step? policy s action with
      | some s' => replay? policy s' rest
      | none => none

theorem step_sound
    {policy : FailurePolicy}
    {pre post : PersistenceState}
    {action : Action}
    (h_step : step? policy pre action = some post) :
    Transition policy pre post := by
  cases action with
  | flush =>
      simp [step?] at h_step
      rcases h_step with ⟨h_pre, h_post⟩
      subst pre
      subst post
      simpa using Transition.flush (policy := policy)
  | writeSuccess =>
      simp [step?] at h_step
      rcases h_step with ⟨h_pre, h_post⟩
      subst pre
      subst post
      simpa using Transition.write_success (policy := policy)
  | writeFail =>
      cases policy with
      | failClosed =>
          simp [step?] at h_step
          rcases h_step with ⟨h_pre, h_post⟩
          subst pre
          subst post
          exact Transition.write_fail_closed rfl
      | failOpen =>
          simp [step?] at h_step
          rcases h_step with ⟨h_pre, h_post⟩
          subst pre
          subst post
          exact Transition.write_fail_open rfl
  | accumulate =>
      simp [step?] at h_step
      rcases h_step with ⟨h_pre, h_post⟩
      subst pre
      subst post
      simpa using Transition.accumulate (policy := policy)

theorem transition_complete
    {policy : FailurePolicy}
    {pre post : PersistenceState}
    (h_trans : Transition policy pre post) :
    ∃ action : Action, step? policy pre action = some post := by
  cases h_trans with
  | flush =>
      exact ⟨.flush, by simp [step?]⟩
  | write_success =>
      exact ⟨.writeSuccess, by simp [step?]⟩
  | write_fail_closed h_policy =>
      exact ⟨.writeFail, by simp [step?, h_policy]⟩
  | write_fail_open h_policy =>
      exact ⟨.writeFail, by simp [step?, h_policy]⟩
  | accumulate =>
      exact ⟨.accumulate, by simp [step?]⟩

theorem replay_sound
    {policy : FailurePolicy}
    {pre post : PersistenceState}
    {actions : List Action}
    (h_replay : replay? policy pre actions = some post) :
    Trace policy pre post := by
  induction actions generalizing pre with
  | nil =>
      simp [replay?] at h_replay
      subst h_replay
      exact Trace.refl
  | cons action rest ih =>
      simp [replay?] at h_replay
      rcases h_step : step? policy pre action with (_ | next)
      · simp [h_step] at h_replay
      · simp [h_step] at h_replay
        have h_trans : Transition policy pre next := step_sound h_step
        exact Trace.step h_trans (ih h_replay)

theorem trace_complete
    {policy : FailurePolicy}
    {pre post : PersistenceState}
    (h_trace : Trace policy pre post) :
    ∃ actions : List Action, replay? policy pre actions = some post := by
  induction h_trace with
  | refl =>
      exact ⟨[], rfl⟩
  | step h_trans h_trace ih =>
      rcases transition_complete h_trans with ⟨action, h_action⟩
      rcases ih with ⟨actions, h_actions⟩
      refine ⟨action :: actions, ?_⟩
      simp [replay?, h_action, h_actions]

end PersistenceState
