import Proofs.Conformance.Functions.Session

namespace CommandPolicy

deriving instance Lean.ToJson for NetworkMode

attribute [conformance_function] effectiveNetworkMode

end CommandPolicy

namespace Conformance.CommandIsolation
open Lean CommandPolicy

structure SeatbeltInput where
  mode : ExecutionMode
  network : NetworkMode
  deriving ToJson

structure SeatbeltOutput where
  credentialsProtected : Bool
  network : NetworkMode
  deriving ToJson

/-- Seatbelt confines only the bounded modes: read-only commands are validated
by argv and unrestricted commands run unsandboxed, so only modes whose
credentials are protected can be launched in a Seatbelt profile. -/
@[conformance_family]
def seatbelt : Family SeatbeltInput SeatbeltOutput where
  inputs :=
    (([.readOnly, .workspaceWrite, .artifactWrite, .unrestricted] : List ExecutionMode).flatMap fun mode =>
      ([.inherit, .disabled, .enabled] : List NetworkMode).map fun network =>
        ({ mode, network } : SeatbeltInput)).filter fun i => protectsCredentials i.mode
  model := fun i =>
    { credentialsProtected := protectsCredentials i.mode
      network := effectiveNetworkMode i.mode i.network }

end Conformance.CommandIsolation
