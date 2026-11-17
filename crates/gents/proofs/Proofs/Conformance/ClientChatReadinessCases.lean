import Proofs.Clients.ClientShell.ChatReadiness
import Proofs.Conformance.Functions.Clients

namespace ClientChatReadiness
attribute [conformance_function] causeToken
end ClientChatReadiness

namespace Conformance.ChatReadinessCases
open Lean _root_.ClientChatReadiness

structure RouteInput where
  operatorEndpoint : Bool
  bindingMatches : Bool
  deriving ToJson

structure FenceInput where
  route : Option RouteInput
  deriving ToJson

structure FenceOutput where
  refusal : Option Cause
  deriving ToJson

private def fenceInputs : List FenceInput := fenceRoutes.map fun route =>
  { route := route.map fun route => ⟨route.operatorEndpoint, route.bindingMatches⟩ }

private def fenceModel (i : FenceInput) : FenceOutput :=
  { refusal := match chatReadiness (i.route.map fun route => ⟨route.operatorEndpoint, route.bindingMatches⟩) with
      | .ready => none | .notReady cause => some cause }

@[conformance_family]
def writeFence : Family FenceInput FenceOutput where
  inputs := fenceInputs
  model := fenceModel

end Conformance.ChatReadinessCases
