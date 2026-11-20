import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household

structure PairingOverlayInput where
  revoked : Bool
  actor : String
  permission : String
  deriving ToJson

@[conformance_family]
def pairingOverlay : Family PairingOverlayInput Bool where
  inputs := ([false, true] : List Bool).flatMap fun revoked =>
    (["user", "runtime", "agent", "sibling", "outsider"] : List String).flatMap fun actor =>
      (["read", "update"] : List String).map fun permission => { revoked, actor, permission }
  model := fun i =>
    let state := if i.revoked then Household.apply Configuration.agentPairingFixture
      (.revoke ⟨"application_pairings", "overlay", "parent", .object "nodes" "node"⟩)
      else Configuration.agentPairingFixture
    Household.allowed 5 state i.actor "application_pairings" "overlay" i.permission

end Conformance.Household
