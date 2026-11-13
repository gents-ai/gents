import Proofs.Session.SessionQueue.InterruptCases
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Contracts.Machines.Catalog
import Proofs.Conformance.Family

namespace Conformance.InterruptRequestDrain

open Lean SessionQueue SessionQueue.InterruptCases

inductive Outcome where
  | latched
  | missing
  deriving ToJson

inductive DrainReason where
  | activeInterrupted
  deriving ToJson

structure Row where
  foreign : Bool
  scheduled : Bool
  wake : Bool
  deriving ToJson

structure Input where
  active : Bool
  rows : List Row
  deriving ToJson

structure RowObservation where
  state : RequestState
  reason : Option DrainReason
  deriving ToJson

structure Output where
  outcome : Outcome
  latched : Bool
  rows : List RowObservation
  deriving ToJson

def entryOf (index : Nat) (row : Row) : ScopedRow :=
  let origin : ExecutionOrigin := if row.scheduled then .scheduled else .interactive
  { foreign := row.foreign
    entry := if row.wake then { wake (301 + index) with origin }
      else { user with requestId := 301 + index, origin } }

def observe (drained : Bool) : RowObservation :=
  if drained then { state := .interrupted, reason := some .activeInterrupted }
  else { state := .pending, reason := none }

@[conformance_family]
def drain : Family Input Output where
  inputs :=
    [ ⟨true, [⟨false, true, true⟩, ⟨true, true, true⟩, ⟨false, false, true⟩,
        ⟨false, true, false⟩]⟩
    , ⟨false, [⟨false, true, true⟩]⟩
    , ⟨true, []⟩
    , ⟨true, [⟨true, true, true⟩, ⟨false, false, false⟩]⟩ ]
  model := fun i =>
    let rows := i.rows.zipIdx.map fun (row, index) => entryOf index row
    match interruptRows (if i.active then 100 else 999) rows with
    | some (latched, drained) =>
        { outcome := .latched, latched, rows := drained.map observe }
    | none =>
        { outcome := .missing, latched := false, rows := rows.map fun row =>
          observe (decide (row.entry.requestId ∈ (queueOf row.foreign rows).terminal)) }

end Conformance.InterruptRequestDrain
