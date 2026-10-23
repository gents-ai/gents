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

/-- An operator-created Goal has no opening request until its first fresh
continuation is committed with the physical binding in the same transaction.
The signed opening uses its self sentinel; replay recognizes that physical
child without rewriting its input. A receipt cannot repair an unbound Goal,
and an unrelated predecessor cannot acquire a bound assignment. -/
def continuationReference? (goalId : String) (current : Option String)
    (parentDoc : String) (parent : Option Enrollment.GoalAssignmentInput)
    (receiptDoc : Option String) : Option Enrollment.GoalAssignmentInput :=
  if goalId.isEmpty || parentDoc.isEmpty || current.any String.isEmpty ||
      receiptDoc.any String.isEmpty || receiptDoc == some parentDoc then none
  else match current with
  | none => if receiptDoc.isSome || parent.isSome then none else some ⟨goalId, none⟩
  | some root =>
      if receiptDoc == some root then
        if parent.isSome then none else some ⟨goalId, none⟩
      else if (match parent with
        | none => parentDoc == root
        | some reference => reference.goalId == goalId &&
            reference.rootRequestDocId.getD parentDoc == root) then
        some ⟨goalId, some root⟩
      else none

theorem continuation_receipt_cannot_bind_an_unbound_goal
    (goalId parentDoc receipt : String) (parent : Option Enrollment.GoalAssignmentInput) :
    continuationReference? goalId none parentDoc parent (some receipt) = none := by
  simp [continuationReference?]

theorem fresh_continuation_opens_with_self_reference (goalId parentDoc : String)
    (goalValid : goalId.isEmpty = false) (parentValid : parentDoc.isEmpty = false) :
    continuationReference? goalId none parentDoc none none = some ⟨goalId, none⟩ := by
  simp [continuationReference?, goalValid, parentValid]

/-- The signed physical edge permits a new self-attributed opening or an
untagged physical opening's child. A signed assignment can only extend its
existing root. Retained untagged edges remain ancestry, not assignment evidence. -/
def continuationEdge (goalId parentDoc : String)
    (parent child : Option Enrollment.GoalAssignmentInput) : Bool :=
  match parent with
  | some reference => reference.goalId == goalId && child == some
      { reference with rootRequestDocId := some (reference.rootRequestDocId.getD parentDoc) }
  | none => child.all fun reference => reference.goalId == goalId &&
      reference.rootRequestDocId.all (· == parentDoc)

theorem opening_edge_is_signed_self (goalId parentDoc : String) :
    continuationEdge goalId parentDoc none (some ⟨goalId, none⟩) = true := by
  simp [continuationEdge]

theorem tagged_parent_cannot_drop_assignment (goalId parentDoc : String)
    (reference : Enrollment.GoalAssignmentInput) :
    continuationEdge goalId parentDoc (some reference) none = false := by
  simp [continuationEdge]

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
