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

/-- A direct authenticated database read supplies the documents visible to the
caller. Hidden and missing rows are indistinguishable; enrollment and requester
labels do not grant or restrict visibility beyond native ACP. Both the session
and active request must be uniquely visible for live activity to be exposed. -/
def sourceVisible (sessions requests : Nat) (active : Bool) : Bool :=
  sessions == 1 && requests == 1 && active

theorem invisible_session_hides_activity (requests : Nat) (active : Bool) :
    sourceVisible 0 requests active = false := by rfl

theorem inactive_request_hides_activity (sessions requests : Nat) :
    sourceVisible sessions requests false = false := by simp [sourceVisible]

inductive ActivityResponse where
  | forbidden
  | unavailable
  | visible
  deriving DecidableEq, Repr

/-- The activity route's native node gate. DefraDB's HTTP layer requires
`read-document` for every route it does not list, but routes Gents merges into
the server sit outside that layer, so the read owner applies the same native
node permission, as the caller holds it, before consulting documents. A caller
holding no node read, such as a member whose enrollment grant was revoked, is
refused distinguishably from unavailable activity; holding it grants no
document visibility. -/
def activityResponse (holdsNodeRead : Bool) (sessions requests : Nat) (active : Bool) :
    ActivityResponse :=
  if !holdsNodeRead then .forbidden
  else if sourceVisible sessions requests active then .visible
  else .unavailable

theorem without_node_read_activity_is_forbidden (sessions requests : Nat) (active : Bool) :
    activityResponse false sessions requests active = .forbidden := by
  simp [activityResponse]

theorem node_read_grants_no_document_visibility (sessions requests : Nat) (active : Bool) :
    activityResponse true sessions requests active = .visible ↔
      sourceVisible sessions requests active = true := by
  unfold activityResponse
  cases sourceVisible sessions requests active <;> simp

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
