import Proofs.Clients.ClientShell.RequestCacheIdentity
import Proofs.Conformance.Family

namespace Conformance.ClientRequestCacheIdentity

open Lean ClientShell.RequestCacheIdentity

deriving instance ToJson for ClientShell.SessionTurn.RowState, ClientShell.RequestCacheIdentity.Row

structure MergeInput where
  name : String
  current : List ClientShell.RequestCacheIdentity.Row
  incoming : List ClientShell.RequestCacheIdentity.Row
  selectedDoc : Option Nat
  deriving ToJson

structure MergeOutput where
  rows : List ClientShell.RequestCacheIdentity.Row
  turnDoc : Option Nat
  deriving ToJson

private def docKey (row : ClientShell.RequestCacheIdentity.Row) : Nat :=
  (row.doc.map (· + 1)).getD 0

/-- Rows in document order: the merged store is a set of physical rows. -/
@[conformance_family]
def merge : Family MergeInput MergeOutput where
  inputs := identityCases.map fun (name, current, incoming, selectedDoc) =>
    { name, current, incoming, selectedDoc }
  model := fun i =>
    let rows := mergeRows i.current i.incoming
    { rows := rows.mergeSort fun a b => docKey a ≤ docKey b
      turnDoc := i.selectedDoc.bind (turnDocument rows) }

end Conformance.ClientRequestCacheIdentity
