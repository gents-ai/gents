import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Family

namespace Conformance.WorkspaceCapability

open Lean Identity Identity.WorkspaceAndFileRequests

structure PublicationInput where
  existing : String
  desired : String
  deriving ToJson

structure PublicationOutput where
  revokedWriterRestored : Bool
  revokedReaderRestored : Bool
  stalePublicationAllowed : Bool
  abortedDocumentVisible : Bool
  effectAfterFailedReservation : Bool
  deriving ToJson

@[conformance_family]
def publication : Family PublicationInput PublicationOutput where
  inputs := [{ existing := "cleaning", desired := "ready" }]
  model := fun i =>
    { revokedWriterRestored := (reuseWorkspace (revokeWorkspaceWriter (publishWorkspace true))).writer
      revokedReaderRestored := (reuseWorkspace (revokeWorkspaceReader (publishWorkspace true))).reader
      stalePublicationAllowed := callbackWorkspacePublishable i.existing i.desired
      abortedDocumentVisible := (publishWorkspace false).document
      effectAfterFailedReservation := mayProvisionWorkspace false }

structure ChildAccessInput where
  actor : String
  permission : String
  deriving ToJson

@[conformance_family]
def childAccess : Family ChildAccessInput Bool where
  inputs := ["user", "creator", "writer", "integrator", "reader", "other"].flatMap fun actor =>
    ["read", "update", "delete"].map fun permission => { actor, permission }
  model := fun i => workspaceChildPermission i.actor i.permission

structure ChildRevocationInput where
  actor : String
  deriving ToJson

structure ChildRevocationOutput where
  read : Bool
  deriving ToJson

@[conformance_family]
def childRevocation : Family ChildRevocationInput ChildRevocationOutput where
  inputs := ["user", "creator", "writer", "integrator", "reader", "other"].map fun actor =>
    { actor }
  model := fun i =>
    { read := Household.allowed 8 revokedWorkspaceChildFixture i.actor "workspace_children"
        "binding" "read" }

structure AdmissionInput where
  complete : Bool
  occupied : Bool
  deriving ToJson

@[conformance_family]
def admission : Family AdmissionInput Bool where
  inputs := [false, true].map fun occupied => { complete := true, occupied }
  model := fun i => admitExclusive i.complete i.occupied

structure CleanupInput where
  name : String
  allowed : Bool
  active : Bool
  deriving ToJson

@[conformance_family]
def cleanup : Family CleanupInput Bool where
  inputs := [("active", true, true), ("unauthorized", false, false), ("available", true, false)].map
    fun (name, allowed, active) => { name, allowed, active }
  model := fun i => mayBeginCleanup i.allowed true i.active

end Conformance.WorkspaceCapability
