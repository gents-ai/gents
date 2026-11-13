import Proofs.Session.Recovery.Sweeps.Requests
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Contracts.Machines.Catalog
import Proofs.Conformance.Family

namespace Conformance.RequestRepairScope

open Lean Recovery

structure Row where
  ownNode : Bool
  leaseExpired : Bool
  interruptRequested : Bool
  deriving ToJson

structure Input where
  rows : List Row
  deriving ToJson

structure RowObservation where
  state : RequestState
  changed : Bool
  deriving ToJson

structure Output where
  repaired : Nat
  rows : List RowObservation
  deriving ToJson

def claimed (row : Row) : ScopedRecoveryRow :=
  { ownNode := row.ownNode
    row :=
      { request :=
          { state := .claimed, origin := .interactive, backend := ⟨""⟩
            admission := .executing, deadline := 100, claimTime := 0, currentTime := 10
            retryCount := 0, maxRetries := 3, messageSeq := 0, persistence := .uncommitted }
        leaseExpired := row.leaseExpired
        interruptRequested := row.interruptRequested } }

@[conformance_family]
def sweep : Family Input Output where
  inputs :=
    [ ⟨[⟨true, true, false⟩, ⟨false, true, false⟩]⟩
    , ⟨[⟨true, true, true⟩, ⟨false, true, true⟩]⟩
    , ⟨[⟨true, false, false⟩, ⟨false, false, false⟩]⟩
    , ⟨[⟨true, true, false⟩, ⟨true, false, true⟩, ⟨true, true, true⟩]⟩ ]
  model := fun i =>
    let rows := i.rows.map claimed
    { repaired := sweepRepaired rows
      rows := rows.map fun entry =>
        { state := (sweepRow entry).request.state, changed := sweepRepairs entry } }

end Conformance.RequestRepairScope
