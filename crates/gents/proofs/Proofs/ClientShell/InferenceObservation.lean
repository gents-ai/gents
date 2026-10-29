import Proofs.ClientShell.Types

namespace ClientInferenceObservation

structure Target where
  node : String
  session : String
  request : String
  generation : String
  deriving DecidableEq, Repr

inductive Activity where
  | awaitingProvider | working | retrying | waiting | unavailable
  deriving DecidableEq, Repr

structure Observation where
  target : Target
  turn : Nat
  attempt : Nat
  sequence : Nat
  observedAt : Nat
  activity : Activity
  deriving DecidableEq, Repr

structure State where
  epoch : Nat
  target : Option Target
  terminal : Bool
  observation : Option Observation
  deriving DecidableEq, Repr

/-- Epochs fence asynchronous reads across selection and reconnect. The target
is supplied by the authorized session and request owner; an activity update is
never evidence that a request remains active. Sequence numbers belong to one
runtime generation and order all attempt observations within that generation. -/
def select (state : State) (target : Option Target) (terminal : Bool) : State :=
  { epoch := state.epoch + 1, target, terminal, observation := none }

def receive (state : State) (epoch : Nat) (incoming : Observation) : State :=
  if state.epoch = epoch ∧ !state.terminal ∧ state.target = some incoming.target ∧
      (state.observation.map (·.sequence)).getD 0 < incoming.sequence ∧
      ((state.observation.map (·.turn)).getD 0 < incoming.turn ∨
        ((state.observation.map (·.turn)).getD 0 = incoming.turn ∧
         (state.observation.map (·.attempt)).getD 0 ≤ incoming.attempt)) then
    { state with observation := some incoming }
  else state

theorem terminal_ignores_observation (state : State) (epoch : Nat) (incoming : Observation)
    (h : state.terminal = true) : receive state epoch incoming = state := by
  simp [receive, h]

theorem old_epoch_ignored (state : State) (epoch : Nat) (incoming : Observation)
    (h : state.epoch ≠ epoch) : receive state epoch incoming = state := by
  simp [receive, h]

theorem wrong_target_ignored (state : State) (epoch : Nat) (incoming : Observation)
    (h : state.target ≠ some incoming.target) : receive state epoch incoming = state := by
  simp [receive, h]

theorem reordered_observation_ignored (state : State) (epoch : Nat) (incoming : Observation)
    (h : incoming.sequence ≤ (state.observation.map (·.sequence)).getD 0) :
    receive state epoch incoming = state := by
  simp [receive, Nat.not_lt.mpr h]

theorem older_attempt_ignored (state : State) (epoch : Nat) (incoming : Observation)
    (sameTurn : (state.observation.map (·.turn)).getD 0 = incoming.turn)
    (h : incoming.attempt < (state.observation.map (·.attempt)).getD 0) :
    receive state epoch incoming = state := by
  simp [receive, sameTurn, Nat.not_le.mpr h]

/-- Freshness is measured in a local monotonic time domain. A missing,
expired, future-dated or terminal observation cannot assert provider health. -/
def project (state : State) (now maxAge : Nat) : Option Activity := do
  if state.terminal then none else
  let observation ← state.observation
  if observation.observedAt ≤ now ∧ now - observation.observedAt ≤ maxAge then
    some observation.activity
  else none

theorem selection_clears_observation (state : State) (target : Option Target) (terminal : Bool) :
    (select state target terminal).observation = none := by rfl

theorem selection_fences_pending_read (state : State) (target : Option Target)
    (terminal : Bool) (incoming : Observation) :
    receive (select state target terminal) state.epoch incoming = select state target terminal := by
  simp [receive, select]

end ClientInferenceObservation
