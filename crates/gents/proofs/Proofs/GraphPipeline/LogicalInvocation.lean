import Proofs.GoalAutomation
import Proofs.GraphPipeline.FailureAttribution

/- IDs denote physical documents, not timestamps. Authentication is
   established by the existing signed physical-edge validator. No new persisted
   identity or Goal policy is introduced. Finite safety, not scheduler liveness. -/
namespace GraphPipeline.LogicalInvocation
abbrev Doc := Nat
structure Attempt where
  doc : Doc
  pinnedRoot : Bool
  terminal : Option Goals.RequestTerminal
  deriving DecidableEq, Repr
structure Edge where
  parent : Doc
  child : Doc
  authenticated : Bool
  deriving DecidableEq, Repr

def parents (edges : List Edge) (child : Doc) : List Doc :=
  (edges.filter (fun e => e.authenticated && e.child == child)).map Edge.parent

/-- Finite paths include their root. Fuel is row count, sufficient for an
acyclic unique-document graph; exhausted/ambiguous paths fail closed. -/
def rootsAt (rows : List Attempt) (edges : List Edge) : Nat → Doc → List Doc
  | 0, _ => []
  | n+1, doc =>
      let direct := if rows.any (fun a => a.doc == doc && a.pinnedRoot) then [doc] else []
      direct ++ (parents edges doc).flatMap (rootsAt rows edges n)

def roots (rows : List Attempt) (edges : List Edge) (doc : Doc) : List Doc :=
  (rootsAt rows edges (rows.length + 1) doc).eraseDups

def members (rows : List Attempt) (edges : List Edge) (root : Doc) : List Attempt :=
  rows.filter (fun a => roots rows edges a.doc == [root])

def ambiguous (rows : List Attempt) (edges : List Edge) : Bool :=
  rows.any (fun a => (roots rows edges a.doc).length > 1 ||
    (parents edges a.doc).eraseDups.length > 1)

def tips (rows : List Attempt) (edges : List Edge) (root : Doc) : List Attempt :=
  let ms := members rows edges root
  ms.filter (fun a => !(edges.any (fun e => e.authenticated && e.parent == a.doc &&
    ms.any (fun child => child.doc == e.child))))

structure GoalEvidence where
  status : Goals.Status
  phase : GoalAutomation.ContinuationPhase
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving DecidableEq, Repr

/-- Graph observes committed Goal ownership, never replays GoalSource's decision
inputs or retry policy. Active includes undecided and claimed publication gaps.
The existing graph deadline bounds an unavailable Goal owner. Caller binds this
snapshot to the canonical physical Goal and authenticated invocation. -/
def obligation (g : GoalEvidence) : Bool :=
  g.status == .active ||
    (g.status == .budgetLimited && g.wrapupRequested && !g.wrapupCompleted)

inductive Outcome where
  | outstanding | succeeded | failed (tip : Doc) | invalid | limitExceeded
  deriving DecidableEq, Repr

def project (rows : List Attempt) (edges : List Edge) (root : Doc)
    (goal : Option GoalEvidence) (resultSatisfied : Bool) : Outcome :=
  if ambiguous rows edges then .invalid
  else if (members rows edges root).any (fun a => a.terminal.isNone) then .outstanding
  else match tips rows edges root with
  | [tip] => match tip.terminal with
    | none => .outstanding
    | some terminal =>
      if goal.any obligation then .outstanding
      else if terminal == .completed && resultSatisfied then .succeeded
      else .failed tip.doc
  | _ => .invalid

/-- Historical ancestry survives Goal replacement. Only matching current head
binding grants the canonical Goal's continuation obligation. -/
def associatedGoal (goal : Option GoalEvidence) (headBindingMatches : Bool) : Option GoalEvidence :=
  if headBindingMatches then goal else none

theorem replaced_goal_does_not_grant_obligation (g : GoalEvidence) :
    associatedGoal (some g) false = none := rfl

/-- Limits count physical members, never logical chains. -/
def projectLimited (rows : List Attempt) (edges : List Edge) (root : Doc)
    (goal : Option GoalEvidence) (resultSatisfied : Bool) (maximum : Nat) : Outcome :=
  if (members rows edges root).length > maximum then .limitExceeded
  else project rows edges root goal resultSatisfied

/-- This supplies the existing first-cause owner, not a second latch. -/
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

theorem two_authenticated_roots_fail_closed :
    project [root, {root with doc := 30}, child] [edge, ⟨30,20,true⟩]
      10 none true = .invalid := by decide

/-- Existing GraphRun key is the shared write witness. `children` represents
new pending physical rows in the same transaction, not a persisted authority.
This model assumes same-store write conflict; scans provide no phantom fence.
Fresh unassociated Goal/root discovery is explicitly outside this refinement. -/
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
