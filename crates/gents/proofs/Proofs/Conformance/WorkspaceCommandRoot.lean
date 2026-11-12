import Proofs.Session.CommandPolicy.Sandbox
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.WorkspaceCommandRoot

open Lean CommandPolicy

structure Input where
  mode : ExecutionMode
  configured : Bool
  fullAccess : Bool
  within : Bool
  deriving ToJson

@[conformance_family]
def selection : Family Input Bool where
  inputs :=
    ([.readOnly, .workspaceWrite, .artifactWrite, .unrestricted] : List ExecutionMode).flatMap fun mode =>
    [false, true].flatMap fun configured =>
    [false, true].flatMap fun fullAccess =>
    [false, true].map fun within => { mode, configured, fullAccess, within }
  model := fun i => useWorkspaceRoot i.mode i.configured i.fullAccess i.within

end Conformance.WorkspaceCommandRoot
