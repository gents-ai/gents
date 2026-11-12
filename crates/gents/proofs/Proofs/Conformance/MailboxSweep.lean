import Proofs.Automation.Mailbox.Sweep
import Proofs.Conformance.Family

namespace Mailbox.Sweep

deriving instance Lean.ToJson for Row

end Mailbox.Sweep

namespace Conformance.MailboxSweepContracts
open Lean Mailbox.Sweep

structure SweepInput where
  name : String
  nodeDid : String
  pageSize : Nat
  budget : Nat
  rows : List Row
  ticks : Nat
  /-- A bound, not an expectation: the second tick's index scan reads at most
  the local open rows past its cursor. -/
  secondTickIndexBound : Nat
  deriving ToJson

structure TickObservation where
  cursor : Option Nat
  scanned : Nat
  acted : Nat
  nextCursor : Option Nat
  deriving ToJson

structure RowStatus where
  docId : Nat
  isOpen : Bool
  deriving ToJson

structure SweepOutput where
  ticks : List TickObservation
  finalRows : List RowStatus
  deriving ToJson

private def tick (state : State) (nodeDid : String) (pageSize budget : Nat) : TickObservation :=
  let batch := selected state nodeDid pageSize budget
  { cursor := state.cursor, scanned := batch.length, acted := (batch.filter (·.due)).length
    nextCursor := (advance state nodeDid pageSize budget).cursor }

private def run (state : State) (nodeDid : String) (pageSize budget : Nat) :
    Nat → List TickObservation × State
  | 0 => ([], state)
  | n + 1 =>
    let (rest, final) := run (advance state nodeDid pageSize budget) nodeDid pageSize budget n
    (tick state nodeDid pageSize budget :: rest, final)

private def progressInputs : List Row :=
  (List.range 35).map (fun index =>
    let docId := index + 1
    ⟨docId, "local", true, docId % 4 != 0 && docId != 35⟩) ++
    [⟨36, "foreign", true, true⟩]

private def indexBound (rows : List Row) (nodeDid : String) (pageSize budget : Nat) : Nat :=
  let second := advance { rows, cursor := none } nodeDid pageSize budget
  (second.rows.filter fun row =>
    row.nodeDid == nodeDid && row.isOpen && second.cursor.all (row.docId > ·)).length

@[conformance_family]
def progress : Family SweepInput SweepOutput where
  inputs :=
    [ { name := "close_while_keyset_advances", nodeDid := "local", pageSize := 16, budget := 16
        rows := progressInputs, ticks := 4
        secondTickIndexBound := indexBound progressInputs "local" 16 16 } ]
  model := fun i =>
    let (ticks, final) := run { rows := i.rows, cursor := none } i.nodeDid i.pageSize i.budget i.ticks
    { ticks, finalRows := final.rows.map fun row => { docId := row.docId, isOpen := row.isOpen } }

end Conformance.MailboxSweepContracts
