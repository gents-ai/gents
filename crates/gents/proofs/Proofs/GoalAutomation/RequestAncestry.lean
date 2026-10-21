import Proofs.GoalAutomation
import Proofs.GoalAutomation.RequestHead

/-!
Authenticated physical request ancestry. The Rust owner is
`goal::authenticated_goal_request_members`. IDs denote physical documents, not
timestamps. Fuel is row count, sufficient for an acyclic unique-document graph;
exhausted or ambiguous paths fail closed.
-/
namespace GoalAutomation.RequestAncestry
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

/-- Background wake facts come from the existing signed local-control receipt
and typed queue decoder. They extend assignment observation only; graph and
output-obligation invocation membership retain Goal-only ancestry. -/
structure AssignmentRow where
  request : RequestHead.Row
  backgroundWake : Bool := false
  tagged : Bool := false
  runtimeWakeReceipt : Bool := false
  /-- Derived from retained Goal assignment versions in this read snapshot. -/
  openingGoals : List Nat := []
  deriving DecidableEq, Repr

def scopedRows (owner session doc : Nat) (rows : List AssignmentRow) : List AssignmentRow :=
  rows.filter fun row => row.request.doc == doc &&
    RequestHead.inScope ⟨owner, session, 0⟩ row.request

/-- Untagged background wakes retain their authenticated physical parent when
observing an assignment. Tagged invocations already carry their signed opening
reference. Goal identity remains fixed across either kind of parent edge. -/
def physicalEntryAt (owner session : Nat) (rows : List AssignmentRow)
    (includeWakes : Bool) : Nat → Option Nat → Doc → Option Doc
  | 0, _, _ => none
  | fuel + 1, expectedGoal, doc => do
      let row ← match scopedRows owner session doc rows with
        | [row] => some row
        | _ => none
      if !row.request.receiptValid then none
      else match row.request.goal with
      | some goal =>
          if expectedGoal.isSome && expectedGoal != some goal then none
          else do
            let parentDoc ← row.request.parentDoc
            let parent ← match scopedRows owner session parentDoc rows with
              | [parent] => some parent
              | _ => none
            if RequestHead.verifiedEdge ⟨owner, session, goal⟩ parent.request row.request then
              physicalEntryAt owner session rows includeWakes fuel (some goal) parent.request.doc
            else none
      | none =>
          if includeWakes && expectedGoal.any (fun goal =>
              !row.openingGoals.isEmpty && !row.openingGoals.contains goal) then none
          else if includeWakes && row.backgroundWake && !row.tagged && row.openingGoals.isEmpty then do
            let parentDoc ← row.request.parentDoc
            let parent ← match scopedRows owner session parentDoc rows with
              | [parent] => some parent
              | _ => none
            if row.runtimeWakeReceipt && !parent.tagged &&
                row.request.parentRequest == some parent.request.request &&
                row.request.doc != parent.request.doc then
              physicalEntryAt owner session rows includeWakes fuel expectedGoal parent.request.doc
            else none
          else some row.request.doc

def signedEntryAt (owner session : Nat) (rows : List RequestHead.Row)
    (fuel : Nat) (expectedGoal : Option Nat) (doc : Doc) : Option Doc :=
  physicalEntryAt owner session (rows.map fun row => {request := row}) false fuel expectedGoal doc

def signedEntry (owner session : Nat) (rows : List RequestHead.Row)
    (expectedGoal : Option Nat) (doc : Doc) : Option Doc :=
  signedEntryAt owner session rows (rows.length + 1) expectedGoal doc

def assignmentEntry (owner session : Nat) (rows : List AssignmentRow)
    (expectedGoal : Option Nat) (doc : Doc) : Option Doc :=
  physicalEntryAt owner session rows true (rows.length + 1) expectedGoal doc

theorem other_goal_cannot_extend_signed_path (owner session goal other : Nat)
    (rows : List AssignmentRow) (row : AssignmentRow) (fuel : Nat) (includeWakes : Bool)
    (lookup : scopedRows owner session row.request.doc rows = [row])
    (childGoal : row.request.goal = some other) (different : goal ≠ other) :
    physicalEntryAt owner session rows includeWakes (fuel + 1) (some goal) row.request.doc = none := by
  simp [physicalEntryAt, lookup, childGoal, different]

private def root : Attempt := ⟨10, true, some .failed⟩
private def child : Attempt := ⟨20, false, some .completed⟩
private def edge : Edge := ⟨10, 20, true⟩

/-- An authenticated edge into a pinned entry stays visible through `parents`;
a consumer that requires a parentless root can check it there. -/
theorem pinned_root_with_parent_has_parent : parents [⟨20,10,true⟩] 10 ≠ [] := by decide

/-- Two authenticated pinned roots over one child make `ambiguous` true. -/
theorem two_authenticated_roots_ambiguous :
    ambiguous [root, {root with doc := 30}, child] [edge, ⟨30,20,true⟩] = true := by decide

end GoalAutomation.RequestAncestry
