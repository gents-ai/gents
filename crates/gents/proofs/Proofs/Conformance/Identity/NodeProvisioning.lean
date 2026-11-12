import Proofs.Store.Identity.NodeProvisioning
import Proofs.Conformance.Family

namespace Conformance.NodeProvisioning

open Lean Identity Identity.NodeProvisioning

structure ExposureInput where
  name : String
  nodeCategory : Bool
  agentCategory : Bool
  deriving ToJson

@[conformance_family]
def agentCreationExposure : Family ExposureInput Bool where
  inputs :=
    [("agent-category", false, true), ("node-category", true, false),
      ("both-categories", true, true), ("no-category", false, false)].map
      fun (name, node, agent) =>
        ({ name, nodeCategory := node, agentCategory := agent } : ExposureInput)
  model := fun i => agentCommandExposed i.nodeCategory i.agentCategory

structure ReservationInput where
  name : String
  created : Bool
  fileSynced : Bool
  directorySynced : Bool
  deriving ToJson

def reservationInputs : List ReservationInput :=
  [("confirmed", true, true, true), ("exists-or-create-failed", false, false, false),
    ("file-sync-failed", true, false, false), ("directory-sync-failed", true, true, false)].map
    fun (name, created, fileSynced, directorySynced) =>
      { name, created, fileSynced, directorySynced }

/-- A home can already hold the marker, and a home that cannot be opened for its
directory sync fails that step after the marker is written; nothing makes a
created marker's own file sync fail. -/
@[conformance_family]
def reservation : Family ReservationInput Bool where
  inputs := reservationInputs.filter fun i => !i.created || i.fileSynced
  model := fun i => reservationConfirmed i.created i.fileSynced i.directorySynced

/-- model-production-gap: no production path makes the file sync of a newly
created bootstrap marker fail. -/
@[conformance_family]
def reservationUnreached : Family ReservationInput Bool where
  inputs := reservationInputs.filter fun i => i.created && !i.fileSynced
  model := fun i => reservationConfirmed i.created i.fileSynced i.directorySynced

end Conformance.NodeProvisioning
