import Proofs.Basic

/-!
# Graph pipeline publication boundary

The model-facing endpoint composes existing Tasks with ordinary EventTriggers;
it is not a second execution engine. This model fences the semantic change:
publication is legal only after type, topology, capability-authorization, and
structural-bound checks all succeed. The compiler is pure and publication is
one transaction in Rust.
-/

namespace GraphPipeline

structure Checks where
  typesValid : Bool
  topologyValid : Bool
  capabilitiesAuthorized : Bool
  withinBounds : Bool
  deriving DecidableEq, Repr

def Checks.wholeGraphValid (checks : Checks) : Prop :=
  checks.typesValid = true ∧
    checks.topologyValid = true ∧
    checks.capabilitiesAuthorized = true ∧
    checks.withinBounds = true

instance (checks : Checks) : Decidable checks.wholeGraphValid := by
  unfold Checks.wholeGraphValid
  infer_instance

inductive Status where
  | proposed
  | published
  | rejected
  deriving DecidableEq, Repr

structure State where
  checks : Checks
  status : Status
  deriving DecidableEq, Repr

def State.safe (state : State) : Prop :=
  state.status = .published → state.checks.wholeGraphValid

def initial (checks : Checks) : State :=
  { checks, status := .proposed }

inductive Action where
  | publish
  | reject
  deriving DecidableEq, Repr

def step? (state : State) : Action → Option State
  | .publish =>
      if state.status = .proposed ∧ state.checks.wholeGraphValid then
        some { state with status := .published }
      else
        none
  | .reject =>
      if state.status = .proposed ∧ ¬ state.checks.wholeGraphValid then
        some { state with status := .rejected }
      else
        none

theorem initial_safe (checks : Checks) : (initial checks).safe := by
  simp [initial, State.safe]

theorem step_preserves_checks
    {pre post : State} {action : Action}
    (h : step? pre action = some post) :
    post.checks = pre.checks := by
  cases action with
  | publish =>
      simp only [step?] at h
      split at h
      · injection h with h_state
        rw [← h_state]
      · simp at h
  | reject =>
      simp only [step?] at h
      split at h
      · injection h with h_state
        rw [← h_state]
      · simp at h

theorem publish_requires_whole_graph_valid
    {pre post : State}
    (h : step? pre .publish = some post) :
    pre.checks.wholeGraphValid := by
  simp only [step?] at h
  split at h <;> simp_all

theorem invalid_graph_cannot_publish
    (state : State)
    (h : ¬ state.checks.wholeGraphValid) :
    step? state .publish = none := by
  simp [step?, h]

theorem successful_publish_is_safe
    {pre post : State}
    (h : step? pre .publish = some post) :
    post.safe := by
  have h_valid := publish_requires_whole_graph_valid h
  simp only [step?] at h
  split at h
  · cases h
    simpa [State.safe] using h_valid
  · simp at h

theorem safe_preserved
    {pre post : State} {action : Action}
    (_h_safe : pre.safe)
    (h_step : step? pre action = some post) :
    post.safe := by
  cases action with
  | publish => exact successful_publish_is_safe h_step
  | reject =>
      simp only [step?] at h_step
      split at h_step
      · cases h_step
        simp [State.safe]
      · simp at h_step

end GraphPipeline
