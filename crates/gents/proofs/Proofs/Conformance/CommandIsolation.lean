import Proofs.CommandPolicy.Sandbox
import Lean

namespace Conformance.CommandIsolation
open Lean CommandPolicy

def casesJson : Json := toJson <|
  ([.readOnly, .workspaceWrite, .artifactWrite, .unrestricted] : List ExecutionMode).flatMap fun mode =>
  ([.inherit, .disabled, .enabled] : List NetworkMode).map fun network => Json.mkObj [
    ("mode", toJson mode.toDefraDB), ("network", toJson network.toDefraDB),
    ("effectiveNetwork", toJson (effectiveNetworkMode mode network).toDefraDB),
    ("protectsCredentials", toJson (protectsCredentials mode))]

end Conformance.CommandIsolation
