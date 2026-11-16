import Proofs.Store.Identity.GrantSetsConformance
import Proofs.Conformance.Family

namespace Conformance.IdentityDelegation

open Lean Identity.GrantSets Identity.GrantSetsConformance

structure DelegationInput where
  name : String
  holder : String
  relation : String
  delete : Bool
  deriving ToJson

def DelegationInput.ofCase : String × Recipient × Change → DelegationInput
  | (name, r, c) => { name, holder := r.name, relation := relationName c.relation, delete := c.delete }

def DelegationInput.admitted (i : DelegationInput) : Bool :=
  let recipient := (recipients.find? (·.name == i.holder)).getD .agent
  let relation : Relation := if i.relation == "admin" then .admin else .permission i.relation
  admits (holderOf (receives recipient)) ⟨relation, i.delete, true, false⟩

example : delegationCases.all fun c => (DelegationInput.ofCase c).admitted ==
    admits (holderOf (receives c.2.1)) c.2.2 := by native_decide

@[conformance_family]
def native : Family DelegationInput Bool where
  inputs := delegationCases.map DelegationInput.ofCase
  model := DelegationInput.admitted

/-- The tools-authoring scenario runs on one signed node whose enrolled user
writes Tools documents through desired-state apply; only the enrolled user's
held and schema grants and the household owner's schema grant are Tools
writes it can issue. -/
def toolsAuthoringCases : List DelegationInput :=
  (delegationCases.map DelegationInput.ofCase).filter fun c =>
    c.name ∈ ["enrolled-grants-held", "enrolled-refused-schema", "owner-grants-schema"]

@[conformance_family]
def toolsAuthoring : Family DelegationInput Bool where
  inputs := toolsAuthoringCases
  model := DelegationInput.admitted

end Conformance.IdentityDelegation
