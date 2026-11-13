import Proofs.Automation.Triggers.WorkspaceFire
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.WorkspaceFire

open Lean _root_.WorkspaceFire

attribute [conformance_vocabulary] BindingAuthority.toDefraDB

deriving instance ToJson for BindingAuthority, FireNode, Fire, FireOutcome, FireRow

structure Input where
  owner : FireNode
  fires : List Fire
  deriving ToJson

structure Output where
  outcomes : List FireOutcome
  rows : List FireRow
  deriving ToJson

def singleFires : List (List Fire) :=
  ([none, some .executor, some .foreign] : List (Option FireNode)).flatMap fun namedOwner =>
    ([.readOnly, .readWrite] : List BindingAuthority).map fun authority =>
      [{ namedOwner, authority }]

def pairedFires : List (List Fire) :=
  [ [⟨some .foreign, .readWrite⟩, ⟨some .foreign, .readWrite⟩]
  , [⟨none, .readWrite⟩, ⟨none, .readWrite⟩]
  , [⟨none, .readWrite⟩, ⟨none, .readOnly⟩]
  , [⟨some .foreign, .readOnly⟩, ⟨some .foreign, .readOnly⟩] ]

@[conformance_family]
def materialize : Family Input Output where
  inputs :=
    ([.executor, .foreign] : List FireNode).flatMap fun owner =>
      (singleFires ++ pairedFires).map fun fires => { owner, fires }
  model := fun i =>
    let state := run i.owner i.fires
    { outcomes := state.outcomes, rows := state.rows }

end Conformance.WorkspaceFire
