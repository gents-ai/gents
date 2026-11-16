import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Family

namespace Conformance.WorkspaceRootAccess

open Lean Identity Identity.WorkspaceAndFileRequests

structure AccessInput where
  actor : String
  permission : String
  deriving ToJson

structure AccessOutput where
  expected : Bool
  mayPublish : Bool
  writerExecution : Bool
  abortedDocument : Bool
  writerExecutionAfterRevocation : Bool
  afterWriterRevocation : Bool
  afterParentRevocation : Bool
  afterIntegratorRevocation : Bool
  deriving ToJson

private def rootAllowed (state : Household.State) (i : AccessInput) : Bool :=
  Household.allowed 6 state i.actor "workspace_roots" "root" i.permission

@[conformance_family]
def rootAccess : Family AccessInput AccessOutput where
  inputs := ["user", "runtime", "writer", "integrator", "other"].flatMap fun actor =>
    ["read", "update", "delete"].map fun permission => { actor, permission }
  model := fun i =>
    { expected := workspaceRootPermission i.actor i.permission
      mayPublish := workspaceRootPublicationAllowed i.actor
      writerExecution := workspaceRootExecutionAllowed "writer" i.actor workspaceRootFixture
      abortedDocument := (Identity.NodeFileRequests.publish false).document
      writerExecutionAfterRevocation :=
        workspaceRootExecutionAllowed "writer" i.actor workspaceRootAfterWriterRevocation
      afterWriterRevocation := rootAllowed workspaceRootAfterWriterRevocation i
      afterParentRevocation := rootAllowed workspaceRootAfterRevocation i
      afterIntegratorRevocation := rootAllowed workspaceRootAfterIntegratorRevocation i }

/-- The live delegation scenario reads the root as an Agent the workspace never
selected: the modeled `other` actor. -/
@[conformance_family]
def unrelatedAgentRead : Family AccessInput Bool where
  inputs := [{ actor := "other", permission := "read" }]
  model := fun i => workspaceRootPermission i.actor i.permission

end Conformance.WorkspaceRootAccess
