import Proofs.Automation.EventDelivery.WatcherConformance
import Proofs.Conformance.Family

namespace Conformance.WatcherScopedScan

open Lean EventDelivery.Watcher EventDelivery.WatcherConformance

deriving instance ToJson for ScopedRow
deriving instance ToJson for ScopedFailure

inductive ScanStatus where
  | ok | denied | missingRoot
  deriving ToJson

structure ScanWire where
  actor : String
  status : ScanStatus
  rows : List ScopedRow
  deriving ToJson

structure BatchInput where
  name : String
  activeAgents : List String
  scans : List ScanWire
  legacyOrder : List String
  arrivalOrder : List String
  deriving ToJson

structure BatchOutput where
  rows : List ScopedRow
  failures : List ScopedFailure
  needsRescan : Bool
  deriving ToJson

def ScanWire.ofModel (scan : ScopedScan) : ScanWire :=
  match scan.view with
  | .denied => { actor := scan.actor, status := .denied, rows := [] }
  | .missingRoot => { actor := scan.actor, status := .missingRoot, rows := [] }
  | .rows rows => { actor := scan.actor, status := .ok, rows }

def ScanWire.toModel (scan : ScanWire) : ScopedScan :=
  match scan.status with
  | .denied => ⟨scan.actor, .denied⟩
  | .missingRoot => ⟨scan.actor, .missingRoot⟩
  | .ok => ⟨scan.actor, .rows scan.rows⟩

@[conformance_family]
def batch : Family BatchInput BatchOutput where
  inputs := cases.map fun c =>
    { name := c.name, activeAgents := c.activeAgents, scans := c.scans.map ScanWire.ofModel,
      legacyOrder := c.legacyOrder, arrivalOrder := c.arrivalOrder }
  model := fun i =>
    let collected := scopedCandidates i.activeAgents (i.scans.map ScanWire.toModel)
      i.legacyOrder i.arrivalOrder
    { rows := collected.rows, failures := collected.failures,
      needsRescan := scopedNeedsRescan collected }

end Conformance.WatcherScopedScan
