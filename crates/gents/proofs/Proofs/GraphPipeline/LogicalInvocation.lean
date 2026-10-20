import Proofs.GoalAutomation
import Proofs.GraphPipeline.FailureAttribution
import Proofs.GoalAutomation.RequestAncestry

/- IDs denote physical documents, not timestamps. Finite safety, not scheduler
liveness. Fuel is row count, sufficient for an acyclic unique-document graph;
exhausted/ambiguous paths fail closed. -/
namespace GraphPipeline.LogicalInvocation
export GoalAutomation.RequestAncestry (Doc Attempt Edge parents rootsAt roots members ambiguous tips)
structure RootAuthorization where
  receiptValid : Bool
  routePinned : Bool
  nodeMatches : Bool
  deriving DecidableEq, Repr

def RootAuthorization.admitted (a : RootAuthorization) : Bool :=
  a.receiptValid && a.routePinned && a.nodeMatches

theorem foreign_signed_root_denied (route : Bool) :
    RootAuthorization.admitted ⟨true, route, false⟩ = false := by
  cases route <;> rfl

theorem unpinned_owner_signed_root_denied :
    RootAuthorization.admitted ⟨true, false, true⟩ = false := rfl

structure GoalEvidence where
  status : Goals.Status
  phase : GoalAutomation.ContinuationPhase
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving DecidableEq, Repr

/-- Active includes undecided/claimed publication gaps. -/
def obligation (g : GoalEvidence) : Bool :=
  g.status == .active ||
    (g.status == .budgetLimited && g.wrapupRequested && !g.wrapupCompleted)

inductive Outcome where
  | outstanding | succeeded | failed (tip : Doc) | invalid | limitExceeded
  deriving DecidableEq, Repr

def project (rows : List Attempt) (edges : List Edge) (root : Doc)
    (goal : Option GoalEvidence) (resultSatisfied : Bool) : Outcome :=
  if !(parents edges root).isEmpty || ambiguous rows edges then .invalid
  else if (members rows edges root).any (fun a => a.terminal.isNone) then .outstanding
  else match tips rows edges root with
  | [tip] => match tip.terminal with
    | none => .outstanding
    | some terminal =>
      if goal.any obligation then .outstanding
      else if terminal == .completed && resultSatisfied then .succeeded
      else .failed tip.doc
  | _ => .invalid

def associatedGoal (goal : Option GoalEvidence) (headBindingMatches : Bool) : Option GoalEvidence :=
  if headBindingMatches then goal else none

theorem replaced_goal_does_not_grant_obligation (g : GoalEvidence) :
    associatedGoal (some g) false = none := rfl

structure AssignmentHead where
  doc : Doc
  member : Bool
  authenticRoot : Bool
  assignmentApplied : Bool
  authenticatedContinuation : Bool
  deriving DecidableEq, Repr

def AssignmentHead.eligible (head : AssignmentHead) : Bool :=
  head.member || head.authenticatedContinuation || (head.authenticRoot && head.assignmentApplied)

def assignmentOwnsGoal (rootAssignmentApplied : Bool) (heads : List AssignmentHead) : Bool :=
  rootAssignmentApplied && (heads.reverse.find? AssignmentHead.eligible).any AssignmentHead.member

theorem queued_root_has_no_goal_obligation (heads : List AssignmentHead) :
    assignmentOwnsGoal false heads = false := by simp [assignmentOwnsGoal]

theorem unapplied_other_root_is_not_head (doc : Doc) :
    AssignmentHead.eligible ⟨doc, false, true, false, false⟩ = false := rfl

theorem unrelated_chat_is_not_head (doc : Doc) :
    AssignmentHead.eligible ⟨doc, false, false, false, false⟩ = false := rfl

theorem applied_other_root_replaces_goal_head :
    assignmentOwnsGoal true [⟨10,true,true,true,false⟩, ⟨20,false,true,true,false⟩] = false := by decide

theorem pending_other_root_preserves_goal_head :
    assignmentOwnsGoal true [⟨10,true,true,true,false⟩, ⟨20,false,true,false,false⟩] = true := by decide

def projectLimited (rows : List Attempt) (edges : List Edge) (root : Doc)
    (goal : Option GoalEvidence) (resultSatisfied : Bool) (maximum : Nat) : Outcome :=
  if (members rows edges root).length > maximum then .limitExceeded
  else project rows edges root goal resultSatisfied

def witness : Outcome → Option Nat
  | .failed tip => some tip
  | .invalid | .limitExceeded => some 0 -- abstract contract-drift cause, not a request ID
  | _ => none

def capture (s : FailureAttribution.Snapshot) (expected : Nat) (o : Outcome) :=
  FailureAttribution.capture s expected (witness o)

theorem outstanding_has_no_failure : witness .outstanding = none := rfl

theorem successful_has_no_failure : witness .succeeded = none := rfl

theorem outstanding_cannot_latch (s : FailureAttribution.Snapshot) (n : Nat) :
    capture s n .outstanding = s := by
  simp [capture, witness, FailureAttribution.capture]

private def root : Attempt := ⟨10, true, some .failed⟩
private def child : Attempt := ⟨20, false, some .completed⟩
private def edge : Edge := ⟨10, 20, true⟩
private def active : GoalEvidence := ⟨.active, .unclaimed, false, false⟩
private def complete : GoalEvidence := { active with status := .complete }

theorem active_goal_defers_failed_root :
    project [root] [] 10 (some active) false = .outstanding := by decide

theorem unrelated_interactive_row_preserves_active_goal :
    project [root, ⟨30, false, some .completed⟩] [] 10 (some active) false =
      project [root] [] 10 (some active) false := by decide

theorem claimed_gap_stays_outstanding :
    project [root] [] 10 (some {active with phase := .claimed}) false = .outstanding := by decide

theorem completed_descendant_heals_physical_failure :
    project [root, child] [edge] 10 (some complete) true = .succeeded := by decide

theorem failed_without_goal_is_immediate :
    project [root] [] 10 none false = .failed 10 := by decide

theorem unauthenticated_candidate_cannot_heal :
    project [root, child] [{edge with authenticated := false}] 10 none true = .failed 10 := by decide

theorem paused_failed_tip_cannot_succeed_from_result :
    project [root, {child with terminal := some .failed}] [edge] 10
      (some {active with status := .paused}) true = .failed 20 := by decide

theorem pending_descendant_stays_outstanding :
    project [root, {child with terminal := none}] [edge] 10 (some complete) true = .outstanding := by decide

theorem physical_counts_do_not_collapse :
    (members [root, child] [edge] 10).length = 2 := by decide

theorem pinned_root_with_parent_rejected :
    project [⟨10, true, some .completed⟩, ⟨20, false, some .completed⟩]
      [⟨20, 10, true⟩] 10 none true = .invalid := by decide

theorem two_authenticated_roots_fail_closed :
    project [root, {root with doc := 30}, child] [edge, ⟨30,20,true⟩]
      10 none true = .invalid := by decide

/-- `children` counts new pending physical rows in the same transaction;
same-store write conflict is assumed (scans provide no phantom fence). -/
structure PublicationState where
  graph : FailureAttribution.Snapshot
  children : Nat
  deriving DecidableEq, Repr

def publish (s : PublicationState) (expected : Nat) : PublicationState :=
  if s.graph.run.status = .running ∧ s.graph.run.cancellationRequested = false ∧
      s.graph.primary = none ∧ s.graph.generation = expected then
    { s with graph := {s.graph with generation := expected + 1}
             children := s.children + 1 }
  else s

inductive PublicationEvent where
  | publish (expected : Nat)
  | capture (expected cause : Nat)
  | finish (expected cause : Nat)
  | cancel
  deriving DecidableEq, Repr

def publicationStep (s : PublicationState) : PublicationEvent → PublicationState
  | .publish n => publish s n
  | .capture n cause => {s with graph := FailureAttribution.capture s.graph n (some cause)}
  | .finish n cause => {s with graph := FailureAttribution.finish s.graph n (s.children == 0) (some cause)}
  | .cancel => {s with graph := FailureAttribution.requestCancel s.graph}

theorem stale_publication_noop (s : PublicationState) (n : Nat)
    (h : s.graph.generation ≠ n) : publish s n = s := by
  simp [publish, h]

theorem captured_failure_blocks_publication (s : PublicationState) (n : Nat)
    (h : s.graph.primary ≠ none) : publish s n = s := by
  simp [publish, h]

theorem cancellation_blocks_publication (s : PublicationState) (n : Nat)
    (h : s.graph.run.cancellationRequested = true) : publish s n = s := by
  simp [publish, h]

end GraphPipeline.LogicalInvocation
