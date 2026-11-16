import Proofs.Tools.ToolPolicy.WorkspacePermissions
import Proofs.Conformance.Family

namespace ToolPolicy.WorkspacePermissions

deriving instance Lean.ToJson for Mode, Preference, Key, Entry, ResolutionCase
attribute [conformance_vocabulary] Mode.wireName

end ToolPolicy.WorkspacePermissions

namespace Conformance.WorkspacePermissionResolution

open ToolPolicy.WorkspacePermissions

structure Result where
  resolved : Option Entry
  admits : Bool
  deriving Lean.ToJson

@[conformance_family]
def selection : Family ResolutionCase Result where
  inputs := resolutionCases
  model := fun input => {
    resolved := resolve input.entries input.workspace input.session
    admits := resolutionAdmits input.entries input.workspace input.session input.bound }

end Conformance.WorkspacePermissionResolution
