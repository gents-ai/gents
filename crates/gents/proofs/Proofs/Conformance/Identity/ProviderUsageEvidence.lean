import Proofs.Store.Identity.ProviderUsageEvidence
import Proofs.Conformance.Family

namespace Conformance.ProviderUsageEvidence

open Lean Identity

structure EvidenceInput where
  viewer : String
  call : String
  deriving ToJson

/-- Whether each viewer of `Identity.ProviderUsageEvidence.fixture`, reading as
itself, sees each call, replayed against the native ACP engine loaded with the
generated Household policy. -/
@[conformance_family]
def cases : Family EvidenceInput Bool where
  inputs := ["a", "b", "user", "shared", "runtime"].flatMap fun viewer =>
    ProviderUsageEvidence.calls.map fun call => { viewer, call }
  model := fun i => ProviderUsageEvidence.sees i.viewer i.call

end Conformance.ProviderUsageEvidence
