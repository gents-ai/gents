import Proofs.Store.Identity.Household
import Proofs.Conformance.Family

namespace Conformance.RuntimeUse

open Lean Identity.Household

/-- One runtime use (`RuntimeUse.wire`) after revoking the Node's
`runtime_reader`, the credential's `runtime_writer`, both or neither. -/
structure RevocationInput where
  runtimeUse : String
  revokeNodeReader : Bool
  revokeCredentialWriter : Bool
  deriving ToJson

/-- Whether the hosting runtime still reads the use's document, and whether it
still uses it. -/
structure RevocationObserved where
  hostingRead : Bool
  mayUse : Bool
  deriving ToJson

def revocationObserved (i : RevocationInput) : RevocationObserved :=
  match RuntimeUse.all.find? (·.wire == i.runtimeUse) with
  | some use =>
    { hostingRead := hostingRuntimeReads false use i.revokeNodeReader i.revokeCredentialWriter
    , mayUse := runtimeMayUseFor use i.revokeNodeReader i.revokeCredentialWriter }
  | none => { hostingRead := false, mayUse := false }

@[conformance_family]
def revocation : Family RevocationInput RevocationObserved where
  inputs := RuntimeUse.all.flatMap fun use =>
    [(false, false), (true, false), (false, true), (true, true)].map fun (node, credential) =>
      { runtimeUse := use.wire, revokeNodeReader := node, revokeCredentialWriter := credential }
  model := revocationObserved

end Conformance.RuntimeUse
