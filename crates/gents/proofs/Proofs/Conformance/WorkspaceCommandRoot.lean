import Proofs.CommandPolicy.Sandbox
import Lean

namespace Conformance.WorkspaceCommandRoot
open Lean CommandPolicy

def casesJson : Json := toJson <|
  ([.readOnly, .workspaceWrite, .artifactWrite, .unrestricted] : List ExecutionMode).flatMap fun mode =>
  [false, true].flatMap fun configured =>
  [false, true].flatMap fun full =>
  [false, true].map fun within => Json.mkObj [
    ("mode", toJson mode.toDefraDB), ("configured", toJson configured),
    ("fullAccess", toJson full), ("within", toJson within), ("selectedWorkspace", toJson (useWorkspaceRoot mode configured full within))]

end Conformance.WorkspaceCommandRoot
