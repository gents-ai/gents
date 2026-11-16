import Proofs.Clients.ClientShell.ChatReadiness
import Proofs.Clients.ClientShell.RoutePairing
import Proofs.Conformance.Functions.Clients

namespace ClientChatReadiness

attribute [conformance_function] causeToken nextActionToken

end ClientChatReadiness

namespace Conformance.ChatReadinessCases

open Lean _root_.ClientChatReadiness

/-- Native spellings of a source. `other` is both a source-less and a manual
record. -/
private def sourceSpellings : Source → List (Option String)
  | .localStandard => [some "local-standard"]
  | .enrollment => [some "enrollment"]
  | .other => [none, some "manual"]

private def sourceOf : Option String → Source
  | some "local-standard" => .localStandard
  | some "enrollment" => .enrollment
  | _ => .other

structure RouteInput where
  source : Option String
  operatorEndpoint : Bool
  managed : Bool
  deriving ToJson

structure FenceInput where
  route : Option RouteInput
  deriving ToJson

structure FenceOutput where
  refusal : Option Cause
  deriving ToJson

/-- Each fence route with each native spelling of its source. -/
private def fenceInputs : List FenceInput :=
  fenceRoutes.flatMap fun
    | none => [{ route := none }]
    | some r => (sourceSpellings r.source).map fun source =>
        { route := some { source := source, operatorEndpoint := r.operatorEndpoint, managed := r.managed } }

private def fenceModel (i : FenceInput) : FenceOutput :=
  let toRoute : RouteInput → Route := fun r =>
    { source := sourceOf r.source, operatorEndpoint := r.operatorEndpoint, managed := r.managed }
  { refusal := match chatReadiness (i.route.map toRoute) with
      | .ready => none
      | .notReady cause => some cause }

/-- `PeerRecord::chat_refusal` is asked of a saved record, so only the fence
routes that hold a record; the write fence replays the route-less case. -/
@[conformance_family]
def savedRecord : Family FenceInput FenceOutput where
  inputs := fenceInputs.filter (·.route.isSome)
  model := fenceModel

example : chatReadiness none = .notReady .noRoute := rfl

@[conformance_family]
def writeFence : Family FenceInput FenceOutput where
  inputs := fenceInputs
  model := fenceModel

@[conformance_family]
def writeSelection : Family FenceInput FenceOutput where
  inputs := fenceInputs
  model := fenceModel

deriving instance ToJson for ClientRoutePairing.Leg

structure LegsInput where
  outbound : ClientRoutePairing.Leg
  inbound : ClientRoutePairing.Leg
  deriving ToJson

structure LegsOutput where
  combined : Option Bool
  deriving ToJson

@[conformance_family]
def routePairingLegs : Family LegsInput LegsOutput where
  inputs := ClientRoutePairing.allLegs.flatMap fun outbound =>
    ClientRoutePairing.allLegs.map fun inbound => { outbound, inbound }
  model := fun i => { combined := ClientRoutePairing.combinedLegs i.outbound i.inbound }

end Conformance.ChatReadinessCases
