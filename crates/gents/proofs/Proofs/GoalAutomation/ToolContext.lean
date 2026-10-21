import Proofs.GoalAutomation.RequestAssignment

namespace GoalAutomation.ToolContext

open RequestAssignment

structure Identity where
  document : Nat
  goal : Nat
  root : Nat
  deriving DecidableEq, Repr

inductive Observation where
  | unassigned
  | unknown
  | assigned (identity : Identity)
  deriving DecidableEq, Repr

/-- A verified absence belongs to the calling request, not to the latest chat
selection. Unknown provenance cannot borrow another assignment or create one.
Historical reads remain available after replacement; writes require the exact
current physical document, Goal identity and opening request. -/
def observe (versions : List Version) (entry : Entry) : Observation :=
  match observeHistory versions entry, latestVersions versions with
  | .unassigned, _ => .unassigned
  | .assigned goal root _, some frontier =>
    match frontier.find? (fun v => belongs v.assignment entry &&
        v.assignment.goal == goal && v.assignment.root == some root) with
    | some v => .assigned ⟨v.document, goal, root⟩
    | none => .unknown
  | _, _ => .unknown

def canWrite (observation : Observation) (current : Option Identity) : Bool :=
  match observation with
  | .assigned identity => current == some identity
  | _ => false

def canCreate (observation : Observation) (current : Option Identity) : Bool :=
  match observation with
  | .unassigned => current.isNone
  | .assigned identity => current == some identity
  | .unknown => false

def readOnly (observation : Observation) (current : Option Identity) : Bool :=
  match observation with
  | .assigned _ => !canWrite observation current
  | _ => false

theorem unknown_never_creates (current : Option Identity) :
    canCreate .unknown current = false := rfl

theorem unassigned_cannot_update (current : Option Identity) :
    canWrite .unassigned current = false := rfl

theorem unassigned_cannot_join_existing (current : Identity) :
    canCreate .unassigned (some current) = false := rfl

theorem another_assignment_cannot_write (observed current : Identity)
    (different : observed ≠ current) :
    canWrite (.assigned observed) (some current) = false := by
  simp [canWrite, Ne.symm different]

end GoalAutomation.ToolContext
