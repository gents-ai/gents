import Proofs.Tools.ToolPolicy.OperatorPaths
import Proofs.Conformance.Family
import Lean

namespace Conformance.StagedPermissionObjects

open Lean ToolPolicy.OperatorPaths

deriving instance ToJson for StagedObject

structure RefusalInput where
  object : StagedObject
  deriving ToJson

structure RefusalOutput where
  resolveRefused : Bool
  resolvedVerbatim : Bool
  allowCreateRefused : Bool
  mountRefused : Bool
  deriving ToJson

@[conformance_family]
def refusal : Conformance.Family RefusalInput RefusalOutput where
  inputs := [StagedObject.staged, .stagedAlias, .ordinary].map fun object => { object }
  model := fun i =>
    { resolveRefused := nativePathRefused i.object
      resolvedVerbatim := !(nativePathRefused i.object)
      allowCreateRefused := nativePathRefused i.object
      mountRefused := mountRefused i.object }

end Conformance.StagedPermissionObjects
