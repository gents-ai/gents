import Proofs.Automation.GoalAutomation.Observation
import Proofs.Automation.GoalAutomation.ObservationRecovery
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.GoalObservationContracts

open GoalAutomation.Observation Conformance.Contracts Lean

deriving instance ToJson for GoalAutomation.Observation.Scope,
  GoalAutomation.Observation.GoalReference

def current : Scope := ⟨"node", "agent", "session", "session-document", some "requester"⟩
def goal : GoalReference := ⟨"node", "session", "goal-document", "goal-commit"⟩

structure Case where
  actor : String
  scope : Scope
  unique : Bool
  goal : Option GoalReference
  verified : Bool
  deriving DecidableEq, Repr, ToJson

def cases : List Case :=
  ["agent", "other-agent", "node"].flatMap fun actor =>
  [true, false].flatMap fun unique =>
  [true, false].flatMap fun verified =>
  [current, { current with node := "foreign-node" },
    { current with agent := "other-agent" },
    { current with session := "foreign-session" },
    { current with sessionDocument := "recreated-document" },
    { current with requester := some "foreign-requester" },
    { current with requester := none }].flatMap fun scope =>
  [none, some goal, some { goal with node := "foreign-node" },
    some { goal with session := "foreign-session" },
    some { goal with document := "" }, some { goal with composite := "" }].map fun goal =>
      ⟨actor, scope, unique, goal, verified⟩

structure AcceptanceInput where
  current : Scope
  actor : String
  scope : Scope
  unique : Bool
  goal : Option GoalReference
  verified : Bool
  deriving ToJson

@[conformance_family]
def acceptance : Family AcceptanceInput Bool where
  inputs := cases.map fun c => ⟨current, c.actor, c.scope, c.unique, c.goal, c.verified⟩
  model := fun i => accepts i.current ⟨i.scope, i.goal⟩ i.verified

/-- The cases a served node can replay as a lost notification: the selected
Agent's own verified observation of its current session, whose Goal is then
rewritten or removed without a notification. Other cases never project an
observation, so the periodic rescan has nothing to recover. -/
def recoverable (c : Case) : Bool :=
  c.actor == current.agent && c.scope == current && c.unique && c.verified &&
    c.goal.all (goalMatches current)

structure RecoveryTrace where
  before : Option GoalReference
  after : Option GoalReference
  deriving ToJson

structure RecoveryOutput where
  recovery : RecoveryTrace
  deriving ToJson

@[conformance_family]
def recovery : Family Case RecoveryOutput where
  inputs := cases.filter recoverable
  model := fun c =>
    let previous := some (⟨current,
      some { goal with composite := "previous-commit" }⟩ : Observation)
    let schedule : Background.Observation.State := ⟨30, true⟩
    let input : Background.Observation.Input := ⟨29, false, 60, .quiet⟩
    let before := GoalAutomation.ObservationRecovery.observe schedule input
      c.actor c.scope c.unique c.goal previous
    let after := GoalAutomation.ObservationRecovery.observe schedule { input with now := 30 }
      c.actor c.scope c.unique c.goal previous
    { recovery := { before := before.bind (·.goal), after := after.bind (·.goal) } }

example : (cases.filter recoverable).length = 2 := by native_decide

example : cases.length = 504 := by native_decide

end Conformance.GoalObservationContracts
