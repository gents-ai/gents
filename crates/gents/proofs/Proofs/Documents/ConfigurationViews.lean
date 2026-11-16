import Proofs.Store.Views

namespace ConfigurationViews

open Lean

inductive Collection where
  | node
  | defaultSelection
  | context
  | compaction
  | skill
  | tools
  | target
  | surface
  | page
  | pluginSource
  deriving DecidableEq, Repr, ToJson

def visible (_collection : Collection) (readable sameNode : Bool) : Bool :=
  ReadViews.visible .ownNode "node" "reader"
    (if sameNode then "node" else "other") "" readable

theorem visible_requires_reader_access (collection : Collection) (readable sameNode : Bool)
    (included : visible collection readable sameNode = true) : readable = true := by
  exact ReadViews.visible_requires_readable _ _ _ _ _ _ included

theorem visible_requires_selected_node (collection : Collection) (readable sameNode : Bool)
    (included : visible collection readable sameNode = true) : sameNode = true := by
  cases sameNode <;> simp_all [visible, ReadViews.visible, ReadViews.scopeMatches]

def inventory (collection : Collection) (readable sameNode : Bool) : Bool :=
  visible collection readable sameNode

end ConfigurationViews
