import Proofs.Basic

namespace GoalAutomation.Observation

structure Scope where
  node : String
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

/-- The runtime projects only its unique canonical session and its own Goal.
Mutation authority remains on Goal; an absent Goal is an observed fact. -/
def project? (actor : String) (scope : Scope) (unique : Bool)
    (goal : Option GoalReference) : Option Observation :=
  if actor == scope.node && unique && goal.all (goalMatches scope) then
    some ⟨scope, goal⟩
  else none

/-- `signatureValid` comes from the existing DID verifier over the complete
canonical signed payload. Requester absence is exact, never a wildcard.
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

theorem ambiguous_session_cannot_project (actor : String) (scope : Scope)
    (goal : Option GoalReference) : project? actor scope false goal = none := by
  simp [project?]

end GoalAutomation.Observation
