import Proofs.Tools.ToolPolicy.Configuration
import Proofs.Conformance.Family
import Lean

namespace Conformance.PluginBind

open Lean ToolPolicy

structure BindInput where
  declaresBindDir : Bool
  deriving ToJson

structure BindOutput where
  refused : Bool
  namesBindDir : Bool
  deriving ToJson

@[conformance_family]
def admission : Family BindInput BindOutput where
  inputs := [{ declaresBindDir := false }, { declaresBindDir := true }]
  model := fun i =>
    { refused := !pluginBoundCallAdmitted i.declaresBindDir PluginAccess.read (some PluginAccess.read) false false
      namesBindDir := !i.declaresBindDir }

end Conformance.PluginBind
