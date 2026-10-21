import Proofs.GoalAutomation
import Proofs.Enrollment.RequestInput

namespace GoalAutomation.FirstRequestBinding

/-- The shared Goal/request transaction captures the created physical request.
An existing binding is immutable. Absent signed attribution is retained for
native producers whose existing physical-root owner establishes membership. -/
def root? (goalId : String) (current : Option String) (requestDoc : String)
    (reference : Option Enrollment.GoalAssignmentInput) : Option String :=
  if goalId.isEmpty || requestDoc.isEmpty ||
      reference.any (fun value => value.goalId != goalId || value.rootRequestDocId.isSome) ||
      current.any (· != requestDoc) then none
  else some requestDoc

/-- Task assignments reuse the canonical session Goal identity. The deterministic
initial identity is used only when that session has no Goal document. A later
client replacement must not be renamed to its retired predecessor's identity. -/
def taskGoalId (current : Option String) (initial : String) : String :=
  current.getD initial

def taskBindingValid (selected : String) (observed : Option String) (initial : String) : Bool :=
  selected == taskGoalId observed initial

theorem existing_root_cannot_change (goalId old requestDoc : String)
    (reference : Option Enrollment.GoalAssignmentInput) (different : old ≠ requestDoc) :
    root? goalId (some old) requestDoc reference = none := by
  simp [root?, different]

theorem borrowed_root_cannot_open (goalId requestDoc : String) (current : Option String)
    (reference : Enrollment.GoalAssignmentInput) (borrowed : reference.rootRequestDocId.isSome) :
    root? goalId current requestDoc (some reference) = none := by
  simp [root?, borrowed]

theorem replaced_goal_identity_is_retained (current initial : String) :
    taskGoalId (some current) initial = current := rfl

end GoalAutomation.FirstRequestBinding
