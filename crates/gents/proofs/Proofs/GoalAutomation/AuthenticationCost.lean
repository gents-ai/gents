import Proofs.GoalAutomation.RequestAncestry

namespace GoalAutomation.AuthenticationCost

/-- One iteration of the native authenticated physical-ancestry owner.
An opening verifies its receipt. A Goal edge verifies the current receipt,
then the parent and local-control child receipts through the edge owner.
An untagged background edge verifies the current receipt and its
local-control receipt. Signature checking itself is an external primitive. -/
inductive Visit where
  | opening
  | goalEdge
  | backgroundEdge
  deriving DecidableEq, Repr

def checks : Visit → Nat
  | .opening => 1
  | .goalEdge => 3
  | .backgroundEdge => 2

def pathChecks (visits : List Visit) : Nat := (visits.map checks).sum

theorem visit_checks_bounded (visit : Visit) : checks visit ≤ 3 := by
  cases visit <;> decide

theorem path_checks_linear (visits : List Visit) : pathChecks visits ≤ 3 * visits.length := by
  induction visits with
  | nil => simp [pathChecks]
  | cons visit rest ih =>
    have := visit_checks_bounded visit
    simp only [pathChecks, List.map_cons, List.sum_cons, List.length_cons] at *
    omega

/-- A valid newest-first Goal chain selects its newest execution head before
visiting another candidate. It walks each Goal edge to the opening and then
revalidates that opening's assignment. This is a bound for that workload,
not for arbitrary branch order or all retained histories. -/
def newestChainPlan (edges : Nat) : List Visit := List.replicate edges .goalEdge ++ [.opening]

def newestChainChecks (edges : Nat) : Nat := pathChecks (newestChainPlan edges) + 1

theorem newest_chain_checks_exact (edges : Nat) : newestChainChecks edges = 3 * edges + 2 := by
  induction edges with
  | zero => rfl
  | succ edges ih =>
    simp only [newestChainChecks, newestChainPlan, List.replicate_succ,
      List.cons_append, pathChecks, List.map_cons, List.sum_cons, checks] at *
    omega

theorem newest_chain_checks_linear (edges : Nat) : newestChainChecks edges ≤ 3 * (edges + 1) := by
  rw [newest_chain_checks_exact]
  omega

/-- Rejected candidates can repeat assignment ancestry work. This budget
charges those paths only; selector supersession-edge checks are additional.
The newest-chain bound does not extend to arbitrary selections. -/
def assignmentChecks (paths : List (List Visit)) : Nat :=
  (paths.map fun path => pathChecks path + 1).sum

theorem assignment_checks_bounded (paths : List (List Visit)) :
    assignmentChecks paths ≤ 3 * (paths.map List.length).sum + paths.length := by
  induction paths with
  | nil => simp [assignmentChecks]
  | cons path rest ih =>
    have := path_checks_linear path
    simp only [assignmentChecks, List.map_cons, List.sum_cons, List.length_cons] at *
    omega

end GoalAutomation.AuthenticationCost
