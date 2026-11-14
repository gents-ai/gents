import Proofs.Session.ManagedExec.ToolRoute
import Proofs.Conformance.Family

namespace Conformance.ManagedExecTool

open Lean ManagedExec.ToolRoute

deriving instance ToJson for WorkClass

structure RouteInput where
  toolName : String
  deriving ToJson

structure RouteOutput where
  workClass : Option WorkClass
  deriving ToJson

@[conformance_family]
def route : Family RouteInput RouteOutput where
  inputs := ["list_files", "glob", "grep", "bash", "bash_unrestricted"].map fun toolName =>
    { toolName }
  model := fun i => { workClass := ManagedExec.ToolRoute.route i.toolName }

end Conformance.ManagedExecTool
