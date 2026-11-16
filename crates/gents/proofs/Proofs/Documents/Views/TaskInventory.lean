import Proofs.Store.Views

namespace TaskInventory

structure Row where
  node : String
  agent : String
  task : String
  deriving DecidableEq, Repr

def select (scope : ReadViews.Scope) (node : String) (reader : Option String)
    (readable : String → Row → Bool) (task : Option String) (documents : List Row) : List Row :=
  (ReadViews.rows scope node reader Row.node Row.agent readable documents).filter fun row =>
    task.isNone || task == some row.task

theorem rows_are_readable (scope : ReadViews.Scope) (node : String) (reader : Option String)
    (readable : String → Row → Bool) (task : Option String) (documents : List Row)
    (row : Row) (selected : row ∈ select scope node reader readable task documents) :
    ∃ actor, reader = some actor ∧ readable actor row = true := by
  have member := (List.mem_filter.mp selected).1
  exact ReadViews.rows_subset_readable scope node reader Row.node Row.agent readable documents row member

def selection (callingAgent otherNode otherAgent readerAllowed filterTask : Bool) : List String :=
  let scope := if callingAgent then ReadViews.Scope.callingAgent else .ownNode
  let node := if otherNode then "other-node" else "node"
  let agent := if otherAgent then "other-agent" else "reader"
  let documents := [Row.mk node agent "alpha", Row.mk node agent "zeta"]
  (select scope "node" (some "reader") (fun _ _ => readerAllowed)
    (if filterTask then some "zeta" else none) documents).map Row.task

end TaskInventory
