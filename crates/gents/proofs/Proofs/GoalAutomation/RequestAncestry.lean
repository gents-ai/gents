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

/-- A Goal continuation retains the first Goal encountered while walking its
signed physical parents, including older entries without assignment metadata.
Each edge still uses the existing receipt, scope and deterministic-identity
owner; a valid edge for another Goal cannot extend the same invocation. -/
def signedEntryAt (owner session : Nat) (rows : List RequestHead.Row) :
    Nat → Option Nat → Doc → Option Doc
  | 0, _, _ => none
  | fuel + 1, expectedGoal, doc => do
      let row ← match rows.filter (fun row => row.doc == doc &&
          RequestHead.inScope ⟨owner, session, 0⟩ row) with
        | [row] => some row
        | _ => none
      if !row.receiptValid then none
      else match row.goal with
      | none => some row.doc
      | some goal =>
          if expectedGoal.isSome && expectedGoal != some goal then none
          else do
            let parentDoc ← row.parentDoc
            let parent ← match rows.filter (fun parent => parent.doc == parentDoc &&
                RequestHead.inScope ⟨owner, session, goal⟩ parent) with
              | [parent] => some parent
              | _ => none
            if RequestHead.verifiedEdge ⟨owner, session, goal⟩ parent row then
              signedEntryAt owner session rows fuel (some goal) parent.doc
            else none

def signedEntry (owner session : Nat) (rows : List RequestHead.Row)
    (expectedGoal : Option Nat) (doc : Doc) : Option Doc :=
  signedEntryAt owner session rows (rows.length + 1) expectedGoal doc

theorem other_goal_cannot_extend_signed_path (owner session goal other : Nat)
    (rows : List RequestHead.Row) (row : RequestHead.Row) (fuel : Nat)
    (lookup : rows.filter (fun candidate => candidate.doc == row.doc &&
      RequestHead.inScope ⟨owner, session, 0⟩ candidate) = [row])
    (childGoal : row.goal = some other) (different : goal ≠ other) :
    signedEntryAt owner session rows (fuel + 1) (some goal) row.doc = none := by
  simp [signedEntryAt, lookup, childGoal, different]

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
