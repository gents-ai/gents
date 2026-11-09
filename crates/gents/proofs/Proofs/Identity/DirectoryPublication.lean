import Proofs.Identity.State
import Lean

namespace Identity.DirectoryPublication
open Lean

structure Entry where
  source : String
  node : String
  deriving BEq, DecidableEq, Repr

/-- The directory collection is branchable and unprotected, while Node,
Agent and readiness data are private. DefraDB substitutes the node identity
when no actor is supplied, so that lookup cannot establish anonymous access.
Until there is an authoritative public source, the projection has no desired
entries. Discovery and remote agent delivery depending on it are unavailable. -/
def desiredEntries (_source : String) : List Entry := []

/-- `deleted` lists successful native deletions. A failed or unattempted
deletion remains visible, and only this source's rows are eligible. Existing
foreign publications and copies already replicated elsewhere may remain. -/
def retract (source : String) (entries : List Entry) (deleted : List String) : List Entry :=
  entries.filter (fun entry => entry.source != source || !deleted.contains entry.node)

theorem private_source_publishes_nothing (source : String) : desiredEntries source = [] := by rfl

theorem foreign_entry_survives (source : String) (entries : List Entry) (deleted : List String)
    (entry : Entry) (member : entry ∈ entries) (foreign : entry.source ≠ source) :
    entry ∈ retract source entries deleted := by
  simp [retract, member, foreign]

theorem failed_delete_may_leave_owned_entry :
    retract "local" [⟨"local", "node"⟩] [] = [⟨"local", "node"⟩] := by decide

theorem successful_local_retraction_preserves_foreign :
    retract "local" [⟨"local", "node"⟩, ⟨"foreign", "node"⟩] ["node"] =
      [⟨"foreign", "node"⟩] := by decide

structure Case where
  name : String
  source : String
  existing : List Entry
  deleted : List String

def cases : List Case :=
  [ ⟨"no-prior-publication", "local", [], []⟩
  , ⟨"retract-own-preserve-foreign", "local", [⟨"local", "node"⟩, ⟨"foreign", "node"⟩], ["node"]⟩
  , ⟨"failed-delete", "local", [⟨"local", "first"⟩, ⟨"local", "second"⟩], ["first"]⟩
  , ⟨"foreign-only", "local", [⟨"foreign", "node"⟩], ["node"]⟩ ]

def entryJson (entry : Entry) : Json :=
  Json.mkObj [("source", toJson entry.source), ("node", toJson entry.node)]

def caseJson (c : Case) : Json := Json.mkObj
  [("name", toJson c.name), ("source", toJson c.source),
   ("existing", toJson (c.existing.map entryJson)), ("deleted", toJson c.deleted),
   ("expected_desired", toJson ((desiredEntries c.source).map entryJson)),
   ("expected_remaining", toJson ((retract c.source c.existing c.deleted).map entryJson))]

def casesJson : String := (toJson (cases.map caseJson)).compress

end Identity.DirectoryPublication
