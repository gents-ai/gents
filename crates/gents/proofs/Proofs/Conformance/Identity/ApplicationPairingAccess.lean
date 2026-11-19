import Proofs.Store.Identity.ApplicationPairingAccess
import Proofs.Conformance.Family

namespace Conformance.ApplicationPairingAccess

open Lean Identity

structure AccessInput where
  actor : String
  permission : String
  deriving ToJson

/-- Document ACP on one pairing overlay for every actor of
`Identity.ApplicationPairingAccess.fixture`, replayed against the native ACP
engine loaded with the generated Household policy. -/
@[conformance_family]
def cases : Family AccessInput Bool where
  inputs := ["author", "user", "engineer", "runtime", "sibling"].flatMap fun actor =>
    ["read", "update", "delete"].map fun permission => { actor, permission }
  model := fun i =>
    Household.allowed 8 ApplicationPairingAccess.fixture i.actor "application_pairings" "overlay"
      i.permission

end Conformance.ApplicationPairingAccess
