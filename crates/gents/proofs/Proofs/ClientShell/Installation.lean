import Proofs.ClientShell.ObservationScope

namespace ClientInstallation

inductive Phase where
  | accepting
  | draining
  | released
  deriving DecidableEq, Repr

structure State where
  phase : Phase
  observation : ClientObservationScope.State
  history : Nat
  deriving DecidableEq, Repr

inductive Action where
  | attach
  | detach
  | retire
  | release
  | replace (startupReserved : Bool)
  deriving DecidableEq, Repr

/-- The listener owns admission and its store lock. Retirement stops new
attachments; existing attachments remain counted until their non-cancellable
operations settle through the connection owner. Only then may shutdown release
that store. Replacement additionally requires the existing startup lock. These
locks protect process lifetime, not runtime DID or document authorization. -/
def step (state : State) : Action → Option State
  | .attach =>
      if state.phase = .accepting then
        some { state with observation := ClientObservationScope.step state.observation .attach }
      else none
  | .detach =>
      if state.phase ≠ .released ∧ 0 < state.observation.attachments then
        some { state with observation := ClientObservationScope.step state.observation .detach }
      else none
  | .retire =>
      if state.phase = .accepting then some { state with phase := .draining } else none
  | .release =>
      if state.phase = .draining ∧ state.observation.attachments = 0 then
        some { state with phase := .released }
      else none
  | .replace reserved =>
      if state.phase = .released ∧ state.observation.attachments = 0 ∧ reserved = true then
        some { state with phase := .accepting }
      else none

theorem history_is_preserved (before after : State) (action : Action)
    (h : step before action = some after) : after.history = before.history := by
  cases action <;> simp only [step] at h
  all_goals split at h <;> simp_all
  all_goals cases h; rfl

theorem retirement_preserves_pending_attachments (before after : State)
    (h : step before .retire = some after) : after.observation = before.observation := by
  simp only [step] at h
  split at h <;> simp_all
  cases h
  rfl

theorem release_requires_drained_attachments (before after : State)
    (h : step before .release = some after) : before.observation.attachments = 0 := by
  simp only [step] at h
  split at h <;> simp_all

theorem draining_refuses_new_attachments (state : State) (h : state.phase = .draining) :
    step state .attach = none := by
  simp [step, h]

theorem replacement_requires_released_owner (before after : State) (reserved : Bool)
    (h : step before (.replace reserved) = some after) :
    before.phase = .released ∧ before.observation.attachments = 0 ∧ reserved = true := by
  simp only [step] at h
  split at h <;> simp_all

theorem releasing_preserves_observation_intent (before after : State)
    (h : step before .release = some after) :
    after.observation.requested = before.observation.requested := by
  simp only [step] at h
  split at h <;> simp_all
  cases h
  rfl

end ClientInstallation
