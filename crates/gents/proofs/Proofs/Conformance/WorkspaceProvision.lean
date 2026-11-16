import Proofs.Session.Workspace.Provision
import Proofs.Conformance.Family

namespace Conformance.WorkspaceProvision

open Lean Workspace.Provision

deriving instance ToJson for Result, Target

/-- `priorWorkUnit` is the work unit of an earlier successful creation of the
same workspace; `none` when this is the first creation. -/
structure ProvisionInput where
  priorWorkUnit : Option Nat
  workUnit : Nat
  target : Target
  ceilingEscapes : Bool
  deriving ToJson

structure ProvisionOutput where
  result : Result
  identityMismatch : Bool
  outcomeState : Option String
  storedState : Option String
  storedWorkUnit : Option Nat
  target : Target
  rowCarriesDirtyBase : Bool
  deriving ToJson

def ProvisionInput.situation (i : ProvisionInput) : Situation :=
  { prior := i.priorWorkUnit.map (⟨.ready, ·⟩), workUnit := i.workUnit, target := i.target
    ceilingEscapes := i.ceilingEscapes }

def observe (o : Observation) : ProvisionOutput :=
  { result := o.result, identityMismatch := o.identityMismatch
    outcomeState := o.outcomeState.map (·.toDefraDB)
    storedState := o.stored.map (·.state.toDefraDB)
    storedWorkUnit := o.stored.map (·.workUnit)
    target := o.target, rowCarriesDirtyBase := o.rowCarriesDirtyBase }

@[conformance_family]
def create : Family ProvisionInput ProvisionOutput where
  inputs :=
    [ ⟨none, 1, .absent, false⟩
    , ⟨none, 1, .foreign, false⟩
    , ⟨none, 1, .absent, true⟩
    , ⟨none, 1, .foreign, true⟩
    , ⟨some 1, 1, .checkout, false⟩
    , ⟨some 1, 2, .checkout, false⟩
    , ⟨some 1, 2, .checkout, true⟩ ]
  model := fun i => observe (Workspace.Provision.create i.situation)

end Conformance.WorkspaceProvision
