import Proofs.Conformance.Functions.Session

namespace CommandPolicy

deriving instance Lean.ToJson for NetworkMode

attribute [conformance_function] effectiveNetworkMode

end CommandPolicy

namespace Conformance.CommandIsolation
open Lean CommandPolicy

deriving instance ToJson for WriteScope

structure SeatbeltInput where
  mode : ExecutionMode
  network : NetworkMode
  reachesNetwork : Bool
  deriving ToJson

structure SeatbeltOutput where
  credentialsProtected : Bool
  network : NetworkMode
  writes : WriteScope
  deriving ToJson

/-- The sandbox each kernel-contained mode selects on a host with a backend. -/
def containedSandbox (mode : ExecutionMode) : SandboxKind :=
  match selectSandbox { sandboxBackend := .macosSeatbelt } mode with
  | .selected sandbox => sandbox
  | .denied _ => .policyReadOnly

/-- What the kernel lets a command do under the Seatbelt profile its mode
selects: whether saved credentials stay unreadable and unwritable, whether it
can open a connection, and where it can write. A read-only command is a
network reader or not; a bounded command's network follows its mode alone. -/
@[conformance_family]
def seatbelt : Family SeatbeltInput SeatbeltOutput where
  inputs :=
    (([.inherit, .disabled, .enabled] : List NetworkMode).flatMap fun network =>
      [ ({ mode := .readOnly, network, reachesNetwork := true } : SeatbeltInput)
      , { mode := .readOnly, network, reachesNetwork := false }
      , { mode := .workspaceWrite, network, reachesNetwork := false }
      , { mode := .artifactWrite, network, reachesNetwork := false } ])
  model := fun i =>
    let reaches := kernelNetwork i.mode i.network i.reachesNetwork
    { credentialsProtected := protectsCredentials i.mode
      network := if reaches then .enabled else .disabled
      writes := ((seatbeltProfile (containedSandbox i.mode) reaches).map (·.writes)).getD .nothing }

end Conformance.CommandIsolation
