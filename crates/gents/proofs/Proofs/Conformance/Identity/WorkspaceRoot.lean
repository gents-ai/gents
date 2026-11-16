import Proofs.Store.Identity.WorkspaceAndFileRequests
import Proofs.Conformance.Family

namespace Conformance.WorkspaceRoot

open Lean Identity.WorkspaceAndFileRequests

structure CompletenessInput where
  complete : Bool
  visibleConfigured : Bool
  deriving ToJson

@[conformance_family]
def completeness : Family CompletenessInput Bool where
  inputs := [false, true].flatMap fun complete =>
    [false, true].map fun configured => { complete, visibleConfigured := configured }
  model := fun i => rootFallbackAllowed i.complete i.visibleConfigured

end Conformance.WorkspaceRoot
