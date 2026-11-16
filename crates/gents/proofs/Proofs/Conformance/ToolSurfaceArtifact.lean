import Proofs.Tools.ToolPolicy.ArtifactSelection
import Proofs.Conformance.Family

namespace Conformance.ToolSurfaceArtifact

open Lean ToolPolicy.ArtifactSelection CommandPolicy

structure SelectionInput where
  bashUnrestricted : Bool
  readOnlyCeiling : Bool
  deriving ToJson

structure SelectionOutput where
  staticMode : String
  networkDisabled : Bool
  nativeModes : List String
  deriving ToJson

@[conformance_family]
def selection : Family SelectionInput SelectionOutput where
  inputs :=
    [ { bashUnrestricted := true, readOnlyCeiling := false }
    , { bashUnrestricted := true, readOnlyCeiling := true }
    , { bashUnrestricted := false, readOnlyCeiling := false }
    , { bashUnrestricted := false, readOnlyCeiling := true } ]
  model := fun i =>
    let mode := staticMode i.bashUnrestricted i.readOnlyCeiling .artifactWrite
    { staticMode := mode.toDefraDB
      networkDisabled := staticNetworkDisabled .disabled
      nativeModes := [mode.toDefraDB] }

end Conformance.ToolSurfaceArtifact
