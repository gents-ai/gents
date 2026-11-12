import Proofs.Tools.PeerRegistryDiscovery.RootAdmission
import Proofs.Conformance.Family

namespace Conformance.WorkspaceOverlayCwd

open Lean PeerRegistryDiscovery.RootAdmission

deriving instance ToJson for CwdBinding

structure CwdInput where
  components : List String
  deriving ToJson

private def placementRoot : CanonicalPath := ⟨"operator", ["worktrees", "ws-1"]⟩

private def cwdInputs : List CwdInput :=
  [ ⟨["worktrees", "ws-1"]⟩
  , ⟨["worktrees", "ws-1", "src"]⟩
  , ⟨["worktrees", "ws-1", "src", "deep"]⟩
  , ⟨["worktrees", "ws-1-other"]⟩
  , ⟨["worktrees"]⟩
  , ⟨["other"]⟩
  , ⟨[]⟩ ]

@[conformance_family]
def binding : Conformance.Family CwdInput CwdBinding where
  inputs := cwdInputs
  model := fun i => boundRequestCwd placementRoot ⟨"operator", i.components⟩

end Conformance.WorkspaceOverlayCwd
