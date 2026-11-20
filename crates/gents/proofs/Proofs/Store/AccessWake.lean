import Proofs.Store.DocumentAcpChoice

/-! What wakes a runtime's change watchers (gents#3831). A watcher waits on
its change subscription and then reads its scope as the actor it serves. On a
home on the node's own store a row's access changes apply with the row, so the
row's change event is enough. On an embedded Vera node a write commits before
the provider holds the access it owes: the row's event arrives while the
watcher's read cannot see the row yet, and nothing about the row changes when
its access settles. The interleaving is `tla/AccessWake.tla`. -/

namespace AccessWake

open DocumentAcpChoice (Provider)

/-- What a watcher's subscription delivers. -/
inductive Event where
  /-- Rows in the watched collections changed. -/
  | rowsChanged
  /-- The provider's access state changed. -/
  | accessChanged
  deriving DecidableEq, Repr

/-- What the watcher reads again when an event arrives. -/
inductive Delivery where
  /-- The changed documents the event names. -/
  | changedDocuments
  /-- Everything in its scope. -/
  | wholeScope
  deriving DecidableEq, Repr

/-- Whether a watcher also waits for access changes. External premise: an
embedded Vera node publishes `AcpCacheInvalidated` each time the provider's
verified module state root changes, after its access cache has dropped the
answers of the old root, so a read made after the event is decided on the new
state; a block that changes no access publishes nothing. The node's own store
publishes no such event. Decision: a watcher on a Vera home listens for it, so
a row whose access settles after its change event is read when it settles, not
at the watcher's fallback rescan. -/
def listens : Provider → Bool
  | .nodeStore => false
  | .embeddedVera => true

/-- An access change does not name the rows it makes visible or hides. -/
def delivery : Event → Delivery
  | .rowsChanged => .changedDocuments
  | .accessChanged => .wholeScope

theorem a_home_on_the_node_store_is_woken_as_before : listens .nodeStore = false := rfl

theorem a_vera_home_is_woken_when_access_settles : listens .embeddedVera = true := rfl

theorem an_access_change_reads_the_whole_scope : delivery .accessChanged = .wholeScope := rfl

end AccessWake
