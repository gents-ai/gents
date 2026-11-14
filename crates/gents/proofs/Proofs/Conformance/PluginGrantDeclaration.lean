import Proofs.Pack.PluginGrantDeclaration
import Proofs.Conformance.Family

namespace Conformance.PluginGrantDeclaration

open Lean

deriving instance ToJson for Pack.PluginGrantDeclaration.NetKind

structure EntryInput where
  wildcard : Bool
  literal : Bool
  deriving ToJson

structure DeclarationInput where
  net : Pack.PluginGrantDeclaration.NetKind
  entries : List EntryInput
  httpTimeoutMs : Option Nat
  deriving ToJson

def entryShapes : List (List EntryInput) :=
  [ []
  , [{ wildcard := false, literal := false }]
  , [{ wildcard := false, literal := true }]
  , [{ wildcard := true, literal := false }]
  , [{ wildcard := true, literal := true }]
  , [{ wildcard := false, literal := false }, { wildcard := true, literal := true }] ]

@[conformance_family]
def accepted : Family DeclarationInput Bool where
  inputs := [Pack.PluginGrantDeclaration.NetKind.noNet, .anyHost, .hosts, .rawTcp].flatMap
    fun net => entryShapes.flatMap fun entries =>
      [none, some 0, some 1, some 300].map fun timeout =>
        { net, entries, httpTimeoutMs := timeout }
  model := fun i => Pack.PluginGrantDeclaration.servable
    { net := i.net
      entries := i.entries.map (fun e => (⟨e.wildcard, e.literal⟩ : Pack.PluginGrantDeclaration.Entry))
      httpTimeoutMs := i.httpTimeoutMs }

end Conformance.PluginGrantDeclaration
