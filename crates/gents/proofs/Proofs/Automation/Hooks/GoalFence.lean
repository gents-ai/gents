import Proofs.Session.Hooks.Claim
import Proofs.Automation.Goals
import Proofs.Automation.GoalAutomation.RequestAssignment
import Proofs.Session.Enrollment.RequestAdmission

namespace Hooks.GoalFence

open GoalAutomation.RequestAssignment

def manifest : Manifest := { name := "goals", owns := ["Goal"], bound := 0 }

/-- What core hands the plugin at `request.claim.gate` about the request
being claimed: fields of its row. `clientGoal` is a client admission whose
signed input carries a Goal continuation. -/
structure Candidate where
  doc : Nat
  owner : Nat
  session : Nat
  agent : Nat
  goalTriggered : Bool
  triggerGoal : Option Nat
  clientGoal : Bool
  pending : Bool
  deriving DecidableEq, Repr

/-- Rows the plugin is shown. `goal` is its own canonical Goal for the
session, as the assignment it names. `request` is a core row of the same
session with the entry core's authenticated ancestry owner resolves it to;
signatures and ancestry are verified by core, the plugin holds no key. -/
inductive Fact where
  | goal (assignment : Assignment)
  | request (doc : Nat) (entry : Entry) (agent : Nat)
  deriving DecidableEq, Repr

inductive Query where
  | goal (owner session : Nat)
  | requests (owner session : Nat)
  deriving DecidableEq, Repr

def reads (candidate : Candidate) : List Query :=
  [.goal candidate.owner candidate.session, .requests candidate.owner candidate.session]

def retired : String := "superseded by a newer Task Goal assignment"

def denied : String := "request authorization denied"

/-- The request a stale continuation's retirement names as its successor: the
entry the continuation resolves to and the current assignment's root document
(`request_head::stale_assignment_continuations`, `survivor_request_id` and
`survivor_doc_id`). The model names a request by its document number, so both
are rendered from document numbers. -/
def successor (assignment : Assignment) (found : Entry × Nat) : Successor :=
  ⟨toString found.1.doc, toString (assignment.root.getD found.1.doc)⟩

def goalOf : List (Row Nat Fact) → Option Assignment
  | [⟨_, _, .goal assignment⟩] => some assignment
  | _ => none

def requestAt (rows : List (Row Nat Fact)) (doc : Nat) : Option (Entry × Nat) :=
  rows.findSome? fun row =>
    match row.value with
    | .request named entry agent => if named = doc then some (entry, agent) else none
    | .goal _ => none

/-- The opening request of a bound Goal: present, resolved to itself, and a
member of the assignment. -/
def opening (rows : List (Row Nat Fact)) (assignment : Assignment) : Option (Entry × Nat) :=
  assignment.root.bind fun root =>
    (requestAt rows root).filter fun found => found.1.doc == root && belongs assignment found.1

def clientBinding (candidate : Candidate) (goal : Option Assignment)
    (rows : List (Row Nat Fact)) : Bool :=
  Enrollment.clientGoalClaimBinding goal.isSome
    (goal.any fun assignment => candidate.triggerGoal == some assignment.goal)
    (goal.any fun assignment => (opening rows assignment).isSome)
    (goal.any fun assignment =>
      (opening rows assignment).any fun found => found.2 == candidate.agent)

/-- The Goal continuation fence (`goal::fence_goal_continuation_claim_in_txn`
with `goal::request_head::stale_assignment_continuations` restricted to the
request being claimed) as a verdict. It is assembled from the existing owners:
`Enrollment.clientGoalClaimBinding` for a client continuation and
`RequestAssignment.belongs` for membership of the current assignment.

Finding: no existing model defines this decision as a whole; the three owners
are only joined in Rust. This definition is the first statement of it.

Finding: today the fence itself supersedes the request (a request-owned
mutation called from `goal/request_head.rs`) and publishes the trigger
outcome. Here it answers `retire` and core's shared pending terminal path does
both. That path must run the same dependents at every site; the catalog found
the fence's own write skips the workspace writer release.

A store error or a missing opening aborts the claim today; that is `refuse`. -/
def fence (candidate : Candidate) (rows : List (List (Row Nat Fact))) :
    Outcome ClaimVerdict Nat Fact Empty :=
  match rows with
  | [goalRows, requestRows] =>
      if !candidate.goalTriggered then
        .proceed (if candidate.clientGoal then .reject denied else .allow) []
      else if candidate.clientGoal && !clientBinding candidate (goalOf goalRows) requestRows then
        .proceed (.reject denied) []
      else
        match goalOf goalRows with
        | none => .proceed .allow []
        | some assignment =>
            if assignment.root.isNone || !candidate.pending then .proceed .allow []
            else
              match requestAt requestRows candidate.doc with
              | none => .proceed .allow []
              | some found =>
                  match opening requestRows assignment with
                  | none => .refuse "Goal assignment has no authenticated physical opening"
                  | some _ =>
                      if belongs assignment found.1 &&
                          candidate.triggerGoal == some assignment.goal then
                        .proceed .allow []
                      else .proceed (.retire retired (successor assignment found)) []
  | _ => .refuse "Goal claim fence was not shown its declared rows"

def plugin : Plugin Query Candidate ClaimVerdict Nat Fact Empty :=
  { manifest := manifest, hook := { reads := reads, decide := fence } }

/-- The fence stages nothing: its only consequence is the verdict. -/
theorem fence_stages_nothing (candidate : Candidate) (rows : List (List (Row Nat Fact))) :
    (fence candidate rows).effects = [] := by
  unfold fence
  repeat' split
  all_goals rfl

theorem ordinary_request_passes (candidate : Candidate) (goalRows requestRows : List (Row Nat Fact))
    (ordinary : candidate.goalTriggered = false) (native : candidate.clientGoal = false) :
    fence candidate [goalRows, requestRows] = .proceed .allow [] := by
  simp [fence, ordinary, native]

/-- A client continuation is allowed only with the binding the existing
admission model requires: a visible Goal that matches, bound to a verified
opening of the same agent. -/
theorem client_continuation_requires_the_binding (candidate : Candidate)
    (goalRows requestRows : List (Row Nat Fact)) (client : candidate.clientGoal = true)
    (allowed : fence candidate [goalRows, requestRows] = .proceed .allow []) :
    candidate.goalTriggered = true ∧
      clientBinding candidate (goalOf goalRows) requestRows = true := by
  unfold fence at allowed
  simp only [client, Bool.true_and] at allowed
  cases triggered : candidate.goalTriggered with
  | false => simp [triggered] at allowed
  | true =>
    cases bound : clientBinding candidate (goalOf goalRows) requestRows with
    | false => simp [triggered, bound] at allowed
    | true => exact ⟨rfl, rfl⟩

/-- A pending continuation of a bound Goal is allowed only as a member of the
Goal's current assignment, named for that Goal. -/
theorem bound_continuation_requires_membership (candidate : Candidate)
    (goalRows requestRows : List (Row Nat Fact)) (assignment : Assignment) (root : Nat)
    (found : Entry × Nat)
    (triggered : candidate.goalTriggered = true) (pending : candidate.pending = true)
    (goal : goalOf goalRows = some assignment) (bound : assignment.root = some root)
    (located : requestAt requestRows candidate.doc = some found)
    (allowed : fence candidate [goalRows, requestRows] = .proceed .allow []) :
    belongs assignment found.1 = true ∧ candidate.triggerGoal = some assignment.goal := by
  unfold fence at allowed
  simp only [triggered, Bool.not_true, Bool.false_eq_true, if_false, goal, bound,
    Option.isNone_some, pending, Bool.or_self, located] at allowed
  split at allowed
  · cases allowed
  · split at allowed
    · cases allowed
    · split at allowed
      · rename_i member
        simpa using member
      · cases allowed

/-- A pending continuation that is not a member of the current assignment is
retired, never claimed and never rejected. -/
theorem stale_continuation_is_retired (candidate : Candidate)
    (goalRows requestRows : List (Row Nat Fact)) (assignment : Assignment) (root : Nat)
    (start found : Entry × Nat)
    (triggered : candidate.goalTriggered = true) (native : candidate.clientGoal = false)
    (pending : candidate.pending = true)
    (goal : goalOf goalRows = some assignment) (bound : assignment.root = some root)
    (opened : opening requestRows assignment = some start)
    (located : requestAt requestRows candidate.doc = some found)
    (stale : belongs assignment found.1 = false) :
    fence candidate [goalRows, requestRows] =
      .proceed (.retire retired (successor assignment found)) [] := by
  simp [fence, triggered, native, pending, goal, bound, opened, located, stale]

def observedRoot (entry : Entry) : Nat :=
  match entry.reference with
  | none => entry.doc
  | some reference => reference.root.getD entry.doc

/-- Membership implies the existing Goal status model's claim rule
(`Goals.assignmentAllows`) for the root the entry observes: the fence never
allows what that rule excludes. -/
theorem membership_satisfies_assignmentAllows (assignment : Assignment) (entry : Entry)
    (render : Nat → String) (member : belongs assignment entry = true) :
    Goals.assignmentAllows (assignment.root.map render)
      (some (render (observedRoot entry))) = true := by
  unfold belongs at member
  simp only [Bool.and_eq_true] at member
  obtain ⟨_, rooted⟩ := member
  cases root : assignment.root with
  | none => rfl
  | some bound =>
    cases reference : entry.reference with
    | none =>
      simp only [root, reference, beq_iff_eq] at rooted
      simp [Goals.assignmentAllows, observedRoot, reference, rooted]
    | some named =>
      simp only [root, reference, Bool.and_eq_true, beq_iff_eq] at rooted
      simp [Goals.assignmentAllows, observedRoot, reference, rooted.2]

variable {Core EvA : Type}

/-- Composed with the claim: a retired continuation is superseded by core's
own transition on the state the fence saw, and no plugin row changes. The
supersede the fence performs itself today is this transition. -/
theorem retired_continuation_is_superseded_by_core
    (host : Host Core Query Nat Fact Empty) (applied : List (Plugin Query EvA Unit Nat Fact Empty))
    (actor : Actor) (candidate : Candidate) (appliedEvent : Core → Core → EvA)
    (core : ClaimCore Core) (state : State Core Nat Fact) (next : Core) (named : Successor)
    (decided : plugin.hook.run host candidate state = .proceed (.retire retired named) [])
    (superseded : core.retire retired named state.core = some next) :
    claimStep host [plugin] applied actor (fun _ => candidate) appliedEvent core state =
      .ok ({ state with core := next }, .retired retired named) := by
  simp [claimStep, gate, decided, combine, ClaimVerdict.narrow, ClaimVerdict.severity,
    superseded]

/-- Composed with the claim: if the fence does not allow, core's claim does
not happen, whatever core's own guards would have said. -/
theorem unallowed_continuation_is_not_claimed
    (host : Host Core Query Nat Fact Empty) (applied : List (Plugin Query EvA Unit Nat Fact Empty))
    (actor : Actor) (candidate : Candidate) (appliedEvent : Core → Core → EvA)
    (core : ClaimCore Core) (state final : State Core Nat Fact)
    (refused : plugin.hook.run host candidate state ≠ .proceed .allow []) :
    claimStep host [plugin] applied actor (fun _ => candidate) appliedEvent core state ≠
      .ok (final, .claimed) := by
  intro claimed
  obtain ⟨verdicts, _, gated, allAllow, _, _⟩ := claimed_requires_every_gate_and_core host
    [plugin] applied actor _ appliedEvent core state final claimed
  have decisions := gate_verdicts_are_snapshot_decisions host candidate state [plugin] verdicts
    gated
  cases verdicts with
  | nil => simp at decisions
  | cons verdict rest =>
    simp only [List.map_cons, List.map_nil, List.cons.injEq] at decisions
    rw [allAllow verdict (List.mem_cons_self _ _)] at decisions
    exact refused decisions.1

end Hooks.GoalFence
