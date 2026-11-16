import Proofs.P2P.PairingReconcile.OverlayBinding
import Proofs.Documents.ScopeTemplates.State
import Proofs.Tools.Identity.Configuration
import Proofs.Tools.ToolPolicy.Cases
import Proofs.Conformance.Family

namespace PairingReconcile.OverlayBinding
deriving instance Lean.ToJson for Revision, Binding, Current, Facts, Inactive
end PairingReconcile.OverlayBinding

namespace ScopeTemplates
deriving instance Lean.ToJson for AppCollectionFacts, AppCollectionRefusal
end ScopeTemplates

namespace Conformance.ApplicationPairing

open Lean PairingReconcile.OverlayBinding

structure BindingInput where
  name : String
  history : List Revision
  binding : Binding
  current : Current
  deriving ToJson

private def bindingCase (name : String) (history : List Revision) (binding : Binding)
    (current : Current) : BindingInput :=
  { name, history, binding, current }

/-- Revision histories that renew, revoke, lapse, skip a sequence, change member
or network, or conflict at one sequence, replayed through the Rust
`binding_is_current`. -/
@[conformance_family]
def bindingCases : Family BindingInput Bool where
  inputs :=
    [bindingCase "same-generation" [approval 1 0 100] ⟨"net", "m", 1⟩ ⟨"net", "m", 1⟩,
     bindingCase "renewed" [approval 1 0 100, approval 2 90 200] ⟨"net", "m", 1⟩ ⟨"net", "m", 2⟩,
     bindingCase "renewed-twice" [approval 1 0 100, approval 2 90 200, approval 3 190 300]
       ⟨"net", "m", 1⟩ ⟨"net", "m", 3⟩,
     bindingCase "revoked-then-reapproved" [approval 1 0 100, revocation 2 50, approval 3 60 200]
       ⟨"net", "m", 1⟩ ⟨"net", "m", 3⟩,
     bindingCase "expired-then-reapproved" [approval 1 0 100, approval 2 150 300]
       ⟨"net", "m", 1⟩ ⟨"net", "m", 2⟩,
     bindingCase "rebound-after-reapproval" [approval 1 0 100, revocation 2 50, approval 3 60 200]
       ⟨"net", "m", 3⟩ ⟨"net", "m", 3⟩,
     bindingCase "other-member" [approval 1 0 100] ⟨"net", "m", 1⟩ ⟨"net", "n", 1⟩,
     bindingCase "bound-ahead" [approval 1 0 100] ⟨"net", "m", 2⟩ ⟨"net", "m", 1⟩,
     bindingCase "missing-sequence" [approval 1 0 100, approval 3 90 200]
       ⟨"net", "m", 1⟩ ⟨"net", "m", 3⟩,
     bindingCase "bound-revocation" [approval 1 0 100, revocation 2 50] ⟨"net", "m", 2⟩ ⟨"net", "m", 2⟩,
     bindingCase "other-network" [approval 1 0 100] ⟨"old-net", "m", 1⟩ ⟨"net", "m", 1⟩,
     bindingCase "conflicting-revision" [approval 1 0 100, approval 2 90 200, revocation 2 95]
       ⟨"net", "m", 1⟩ ⟨"net", "m", 2⟩]
  model := fun i => bindingCurrent i.history i.binding i.current

/-- Every combination of the facts one overlay is judged on, replayed through
the Rust `decide_overlay` the reconciler, the tool and the operator listing
share. -/
@[conformance_family]
def contributionCases : Family Facts (Option Inactive) where
  inputs :=
    let bools := [false, true]
    bools.flatMap fun a => bools.flatMap fun b => bools.flatMap fun c => bools.flatMap fun d =>
      bools.flatMap fun e => bools.flatMap fun f => bools.map fun g => ⟨a, b, c, d, e, f, g⟩
  model := decideOverlay

/-- Every combination of the facts that admit one application collection,
replayed through the Rust `decide_app_collection`. -/
@[conformance_family]
def collectionAdmissionCases :
    Family ScopeTemplates.AppCollectionFacts (Option ScopeTemplates.AppCollectionRefusal) where
  inputs :=
    let bools := [false, true]
    bools.flatMap fun managed => bools.flatMap fun hasPolicy => bools.map fun isView =>
      { managed, hasPolicy, isView }
  model := ScopeTemplates.decideAppCollection

structure P2pGrant where
  p2pRead : Bool
  p2pMutate : Bool
  deriving ToJson

structure OperationSource where
  purpose : String
  permission : String
  deriving ToJson

/-- The node permission sources a Tools surface carrying only the given P2P
grants installs, replayed through the Rust tool policy surface. -/
@[conformance_family]
def operationSourceCases : Family P2pGrant (List OperationSource) where
  inputs := [false, true].flatMap fun p2pRead => [false, true].map fun p2pMutate =>
    { p2pRead, p2pMutate }
  model := fun grant =>
    let surface : ToolPolicy.Surface :=
      { ToolPolicy.ContractCases.secureMinimal with
        p2pRead := grant.p2pRead
        p2pMutate := grant.p2pMutate
        queryTools := .none
        writeTools := .none }
    (Identity.Configuration.toolOperationSources surface false).map fun (purpose, permission) =>
      { purpose, permission }

end Conformance.ApplicationPairing
