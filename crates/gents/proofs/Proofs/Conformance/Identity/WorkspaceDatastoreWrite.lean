import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Family

namespace Conformance.WorkspaceDatastoreWrite

open Lean Identity.WorkspaceAndFileRequests

structure WriteInput where
  collection : String
  deriving ToJson

def writeInputs : List WriteInput :=
  (workspaceRuntimeOwnedCollections ++ ["Notes"]).map fun collection => { collection }

@[conformance_family]
def validation : Family WriteInput Bool where
  inputs := writeInputs
  model := fun i => workspaceDatastoreWriteAllowed i.collection

/-- The workspace publication scenario holds a published workspace and calls
a configured create tool for a hidden binding under it; the binding is the
only runtime-owned child that scenario has the parent and fields to name. -/
@[conformance_family]
def toolCall : Family WriteInput Bool where
  inputs := writeInputs.filter (·.collection == "WorkspaceBinding")
  model := fun i => workspaceDatastoreWriteAllowed i.collection

end Conformance.WorkspaceDatastoreWrite
