import Proofs.Basic

namespace GoalAutomation.Observation

structure Scope where
  node : String
  agent : String
  session : String
  sessionDocument : String
  requester : Option String
  deriving DecidableEq, Repr

structure GoalReference where
  node : String
  session : String
  document : String
  composite : String
  deriving DecidableEq, Repr

structure Observation where
  scope : Scope
  goal : Option GoalReference
  deriving DecidableEq, Repr

def goalMatches (scope : Scope) (goal : GoalReference) : Bool :=
  goal.node == scope.node && goal.session == scope.session &&
    !goal.document.isEmpty && !goal.composite.isEmpty

/-- Goal observations are effects of the canonical session's selected agent. That
agent signs and owns the projection; the runtime node identifier remains routing.
The runtime may discover sessions operationally, but cannot substitute its key when
projecting agent data. Mutation authority remains on Goal; absence is an observed fact. -/
def project? (actor : String) (scope : Scope) (unique : Bool)
    (goal : Option GoalReference) : Option Observation :=
  if actor == scope.agent && unique && goal.all (goalMatches scope) then
    some ⟨scope, goal⟩
  else none

/-- `signatureValid` comes from the existing DID verifier over the complete
canonical signed payload, verified against the canonical session's selected Agent DID.
The payload binds that Agent DID. Requester absence is exact creation provenance,
never a wildcard or an additional access grant.
The physical session document prevents replay across a recreated header. -/
def accepts (current : Scope) (observation : Observation) (signatureValid : Bool) : Bool :=
  signatureValid && decide (current = observation.scope) &&
    observation.goal.all (goalMatches current)

theorem unverified_observation_is_rejected (current : Scope) (observation : Observation) :
    accepts current observation false = false := by
  simp [accepts]

theorem another_session_document_is_rejected (current : Scope) (observation : Observation)
    (verified : Bool) (h : current.sessionDocument ≠ observation.scope.sessionDocument) :
    accepts current observation verified = false := by
  have unequal : current ≠ observation.scope := by
    intro equal
    exact h (congrArg Scope.sessionDocument equal)
  simp [accepts, unequal]

theorem another_requester_is_rejected (current : Scope) (observation : Observation)
    (verified : Bool) (h : current.requester ≠ observation.scope.requester) :
    accepts current observation verified = false := by
  have unequal : current ≠ observation.scope := by
    intro equal
    exact h (congrArg Scope.requester equal)
  simp [accepts, unequal]

theorem another_agent_is_rejected (current : Scope) (observation : Observation)
    (verified : Bool) (h : current.agent ≠ observation.scope.agent) :
    accepts current observation verified = false := by
  have unequal : current ≠ observation.scope := by
    intro equal
    exact h (congrArg Scope.agent equal)
  simp [accepts, unequal]

theorem node_cannot_replace_agent (scope : Scope) (unique : Bool)
    (goal : Option GoalReference) (different : scope.node ≠ scope.agent) :
    project? scope.node scope unique goal = none := by
  simp [project?, different]

theorem ambiguous_session_cannot_project (actor : String) (scope : Scope)
    (goal : Option GoalReference) : project? actor scope false goal = none := by
  simp [project?]

end GoalAutomation.Observation
