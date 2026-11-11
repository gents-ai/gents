import Proofs.Store.HomeRetirement
import Proofs.Conformance.Family

namespace Conformance.HomeRetirement

open Lean _root_.HomeRetirement

deriving instance ToJson for Role, Reference, Presence, Disposition, Item, Outcome, Retirement

/-- `passes` is how many groups of the same name the owned entries are
retired in; the model does not read it, so the split changes nothing. -/
structure Scenario where
  disposition : Disposition
  passes : Nat
  items : List Item
  deriving ToJson

def everyRole : List Item :=
  ([Role.inventory, .lock, .foreign].flatMap fun role =>
    [Reference.unreferenced, .otherHome, .exclusion].map fun reference =>
      { role, reference, presence := .file })
  ++ [ { role := .maintenance, reference := .unreferenced, presence := .directory }
     , { role := .maintenance, reference := .otherHome, presence := .directory } ]

def owned (presence : Presence) : Item :=
  { role := .inventory, reference := .unreferenced, presence }

def homes : List (List Item) :=
  [ []
  , everyRole
  , [owned .file, owned .directory, owned .link, owned .vanished]
  , [owned .file, owned .uninspectable, owned .directory]
  , [owned .uninspectable, owned .file]
  , [owned .directory, owned .vanished, owned .file, owned .uninspectable, owned .link]
  , [ { role := .lock, reference := .unreferenced, presence := .file }, owned .file
    , { role := .foreign, reference := .unreferenced, presence := .link }
    , { role := .inventory, reference := .exclusion, presence := .directory } ] ]

@[conformance_family]
def retire : Family Scenario Retirement where
  inputs :=
    (homes.flatMap fun items =>
      [Disposition.archive, .archiveOntoBackup, .delete].map fun disposition =>
        { disposition, passes := 1, items })
    ++ [ { disposition := .archive, passes := 2, items := [owned .directory, owned .file] }
       , { disposition := .delete, passes := 2
           items := [owned .file, owned .uninspectable, owned .file] } ]
  model := fun scenario => _root_.HomeRetirement.retire scenario.disposition scenario.items

end Conformance.HomeRetirement
