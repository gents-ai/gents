import Proofs.Store.Identity.DirectoryPublication
import Proofs.Conformance.Family

namespace Conformance.DirectoryPublication

open Lean

open Identity.DirectoryPublication (desiredEntries retract cases)

deriving instance ToJson for Identity.DirectoryPublication.Entry

structure ReconcileInput where
  source : String
  existing : List Identity.DirectoryPublication.Entry
  deleted : List String
  deriving ToJson

structure ReconcileOutput where
  desired : List Identity.DirectoryPublication.Entry
  remaining : List Identity.DirectoryPublication.Entry
  deriving ToJson

@[conformance_family]
def reconcile : Family ReconcileInput ReconcileOutput where
  inputs := cases.map fun c => { source := c.source, existing := c.existing, deleted := c.deleted }
  model := fun i =>
    { desired := desiredEntries i.source
      remaining := retract i.source i.existing i.deleted }

end Conformance.DirectoryPublication
