import Proofs.Store.Identity.NodeFileRequests
import Proofs.Conformance.Family

namespace Conformance.NodeFileRequests

open Lean Identity Identity.NodeFileRequests

structure AccessInput where
  actor : String
  permission : String
  deriving ToJson

structure AccessOutput where
  allowed : Bool
  revokedAllowed : Bool
  abortedDocument : Bool
  deriving ToJson

@[conformance_family]
def access : Family AccessInput AccessOutput where
  inputs := ["user", "runtime", "agent", "other"].flatMap fun actor =>
    ["read", "update", "delete"].map fun permission => { actor, permission }
  model := fun i =>
    { allowed := requestPermission i.actor i.permission
      revokedAllowed :=
        Household.allowed 5 revokedRuntimeFixture i.actor "configuration" "request" i.permission
      abortedDocument := (publish false).document }

end Conformance.NodeFileRequests
