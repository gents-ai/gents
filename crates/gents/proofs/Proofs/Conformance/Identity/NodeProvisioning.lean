import Proofs.Store.Identity.NodeProvisioning
import Proofs.Store.Identity.NodeProvisioningConformance
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

structure BootstrapInput where
  name : String
  exclusive : Bool
  matchingRuntime : Bool
  authenticatedClient : Bool
  nodeAbsentObserved : Bool
  storeEmptyObserved : Bool
  reservationDurable : Bool
  nacEnabled : Bool
  freshEvidence : Bool
  configurationPublished : Bool
  deriving ToJson

def BootstrapInput.ofModel (name : String) (o : BootstrapObservation) : BootstrapInput :=
  { name, exclusive := o.exclusive, matchingRuntime := o.matchingRuntime,
    authenticatedClient := o.authenticatedClient, nodeAbsentObserved := o.nodeAbsentObserved,
    storeEmptyObserved := o.storeEmptyObserved, reservationDurable := o.reservationDurable,
    nacEnabled := o.nacEnabled, freshEvidence := o.freshEvidence,
    configurationPublished := o.configurationPublished }

def BootstrapInput.toModel (i : BootstrapInput) : BootstrapObservation :=
  ⟨i.exclusive, i.matchingRuntime, i.authenticatedClient, i.nodeAbsentObserved,
    i.storeEmptyObserved, i.reservationDurable, i.nacEnabled, i.freshEvidence,
    i.configurationPublished⟩

/-- Init builds a fresh home, and a home whose marker survived a crash before
its grants; each other observation is a separate refusal inside init that no
test home can be put into without replacing init's own steps. -/
def bootstrapReached : List String := ["fresh", "crash-after-reservation"]

@[conformance_family]
def bootstrapGrant : Family BootstrapInput Bool where
  inputs := (Identity.NodeProvisioningConformance.bootstrapCases.filter
    fun (name, _) => name ∈ bootstrapReached).map fun (name, o) => BootstrapInput.ofModel name o
  model := fun i => bootstrapMayGrant i.toModel

/-- model-production-gap: init decides these inline across its store lock,
runtime match, client authentication, Node and store observation, NAC status and
publication steps; no production function takes the observation. -/
@[conformance_family]
def bootstrapGrantUnreached : Family BootstrapInput Bool where
  inputs := (Identity.NodeProvisioningConformance.bootstrapCases.filter
    fun (name, _) => name ∉ bootstrapReached).map fun (name, o) => BootstrapInput.ofModel name o
  model := fun i => bootstrapMayGrant i.toModel

end Conformance.NodeProvisioning
