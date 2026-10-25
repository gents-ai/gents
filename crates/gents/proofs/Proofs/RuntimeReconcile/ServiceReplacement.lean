namespace RuntimeReconcile.ServiceReplacement

inductive Phase where
  | accepting
  | stopping
  | released
  deriving DecidableEq, Repr

/-- These are process-local observations of existing execution and native
cleanup owners, serialized with admission. They are not persisted request
state, host identity, or permission to replay a submitted request. -/
structure State where
  phase : Phase
  executions : Nat
  nativeOutstanding : Nat
  deriving DecidableEq, Repr

/-- Authentication is established by the existing node DID owner. The build
fence binds the command to the authenticated observation of the running
executable; a filesystem path, PID, or version ordering cannot establish it. -/
structure RetirementAuthority where
  operatorAuthenticated : Bool
  observedBuildMatches : Bool
  deriving DecidableEq, Repr

inductive Action where
  | admit
  | finish
  | nativeStarted
  | nativeReturned
  | retire (authority : RetirementAuthority)
  | release (storeReleased : Bool)
  | replace (startupReserved : Bool)
  deriving DecidableEq, Repr

/-- Retirement and new execution admission share one serialized owner. Busy
retirement is refused rather than cancelling work. Successful retirement
closes admission before ordinary shutdown; replacement waits for the actual
native store claim to be released and holds the existing startup reservation.
The native adapter must establish these observations, not infer them from an
HTTP disconnect, a timeout, or an empty durable request query. -/
def step (state : State) : Action → Option State
  | .admit =>
      if state.phase = .accepting then
        some { state with executions := state.executions + 1 }
      else none
  | .finish =>
      if state.phase ≠ .released ∧ 0 < state.executions then
        some { state with executions := state.executions - 1 }
      else none
  | .nativeStarted =>
      if state.phase ≠ .released then
        some { state with nativeOutstanding := state.nativeOutstanding + 1 }
      else none
  | .nativeReturned =>
      if 0 < state.nativeOutstanding then
        some { state with nativeOutstanding := state.nativeOutstanding - 1 }
      else none
  | .retire authority =>
      if state.phase = .accepting ∧ state.executions = 0 ∧
          authority.operatorAuthenticated = true ∧ authority.observedBuildMatches = true then
        some { state with phase := .stopping }
      else none
  | .release storeReleased =>
      if state.phase = .stopping ∧ state.executions = 0 ∧
          state.nativeOutstanding = 0 ∧ storeReleased = true then
        some { state with phase := .released }
      else none
  | .replace reserved =>
      if state.phase = .released ∧ state.executions = 0 ∧
          state.nativeOutstanding = 0 ∧ reserved = true then
        some { state with phase := .accepting }
      else none

theorem retirement_requires_authenticated_build_fence
    (before after : State) (authority : RetirementAuthority)
    (h : step before (.retire authority) = some after) :
    authority.operatorAuthenticated = true ∧ authority.observedBuildMatches = true := by
  simp only [step] at h
  split at h <;> simp_all

theorem busy_retirement_is_refused (state : State) (authority : RetirementAuthority)
    (busy : 0 < state.executions) : step state (.retire authority) = none := by
  simp [step, Nat.ne_of_gt busy]

theorem stopping_refuses_admission (state : State) (h : state.phase = .stopping) :
    step state .admit = none := by
  simp [step, h]

theorem retirement_preserves_owned_work (before after : State) (authority : RetirementAuthority)
    (h : step before (.retire authority) = some after) :
    after.executions = before.executions ∧
      after.nativeOutstanding = before.nativeOutstanding := by
  simp only [step] at h
  split at h <;> simp_all
  cases h
  exact ⟨rfl, rfl⟩

theorem release_requires_native_completion (before after : State) (storeReleased : Bool)
    (h : step before (.release storeReleased) = some after) :
    before.phase = .stopping ∧ before.executions = 0 ∧
      before.nativeOutstanding = 0 ∧ storeReleased = true := by
  simp only [step] at h
  split at h <;> simp_all

theorem replacement_requires_released_store_and_startup_reservation
    (before after : State) (reserved : Bool)
    (h : step before (.replace reserved) = some after) :
    before.phase = .released ∧ before.executions = 0 ∧
      before.nativeOutstanding = 0 ∧ reserved = true := by
  simp only [step] at h
  split at h <;> simp_all

end RuntimeReconcile.ServiceReplacement
