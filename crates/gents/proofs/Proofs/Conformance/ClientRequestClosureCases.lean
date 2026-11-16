import Proofs.Conformance.ClientShell.Contracts.SessionTurn

namespace Conformance.ClientRequestClosure

open ClientShell.SessionTurn
open Conformance.ClientShellContracts.SessionTurnCases

structure ClosedInput where
  name : String
  rows : List Row
  seedDocs : List Nat
  deriving Lean.ToJson

structure ClosedOutput where
  docs : List Nat
  deriving Lean.ToJson

/-- Each scenario closes from its newest row. A session case without the native
completion premise (`seededReadCases`) closes from its modeled read seeds,
because the native seed query cannot stand for it. -/
def closedCases : List ClosedInput :=
  scenarios.map (fun (name, rows) =>
    { name, rows, seedDocs := rows.getLast?.toList.map (·.doc) }) ++
  seededReadCases.map (fun i =>
    { name := i.name, rows := i.rows,
      seedDocs := (readSeeds i.rows i.selectedDoc i.pageRequestDocs).map (·.doc) })

@[conformance_family]
def closed : Family ClosedInput ClosedOutput where
  inputs := closedCases
  model := fun i =>
    let seeds := i.rows.filter (fun row => i.seedDocs.contains row.doc)
    { docs := (closeReadRows i.rows (i.rows.length + 1) seeds).map (·.doc) }

end Conformance.ClientRequestClosure
