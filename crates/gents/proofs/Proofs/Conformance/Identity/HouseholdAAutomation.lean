import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family

namespace Conformance.HouseholdA

open Lean Identity Identity.Household Identity.Conformance

structure OutcomeInput where
  actor : String
  resource : String
  id : String
  permission : String
  deriving ToJson

structure RequestReadInput where
  role : String
  deriving ToJson

def objectOwner (resource id : String) : String :=
  ((HouseholdFixture.initial.objects.find? fun object =>
    object.resource == resource && object.id == id).map (·.owner)).getD ""

/-- The User owns the home Node, the Agent owns the automated request, and
the outsider is the household's last enrolled actor. -/
def requestReader (role : String) : String :=
  if role == "user" then objectOwner "nodes" "home"
  else if role == "agent" then objectOwner "delivered_requests" "automated-request"
  else HouseholdFixture.initial.enrolled.getLastD ""

/-- The household trace in the order the snapshot fixture emitted it. -/
def householdTrace : List Action :=
  HouseholdFixture.nodeAdministratorChanges ++ HouseholdFixture.runtimeReaderChanges ++
    HouseholdFixture.invokeChanges ++ HouseholdFixture.invokeOnlySessionActions ++
    HouseholdFixture.requestSessionChanges ++ HouseholdFixture.receiptAuthorChanges ++
    HouseholdFixture.matrix ++ HouseholdFixture.managementChanges ++
    HouseholdFixture.selfConfigChanges ++ HouseholdFixture.changes

def firstCheck (actor resource id permission : String) : State → List Action → Option Bool
  | _, [] => none
  | state, action :: rest =>
    match action with
    | .check checked checkedResource checkedId checkedPermission =>
      if checked == actor && checkedResource == resource && checkedId == id &&
          checkedPermission == permission then
        some (allowed 5 state checked checkedResource checkedId checkedPermission)
      else firstCheck actor resource id permission (apply state action) rest
    | _ => firstCheck actor resource id permission (apply state action) rest

def automatedRequestRead (i : RequestReadInput) : Option Bool :=
  firstCheck (requestReader i.role) "delivered_requests" "automated-request" "read"
    HouseholdFixture.initial householdTrace

example : ["user", "agent", "outsider"].all
    (fun role => (automatedRequestRead ⟨role⟩).isSome) = true := by native_decide

end Conformance.HouseholdA

namespace Conformance.Household

open Lean Identity Identity.Household Conformance.HouseholdA

@[conformance_family]
def outcomeReaderAccess : Family OutcomeInput Bool where
  inputs := Automation.outcomeActors.flatMap fun actor =>
    Automation.outcomeDocuments.flatMap fun (resource, id) =>
      Automation.outcomePermissions.map fun permission => { actor, resource, id, permission }
  model := fun i => Household.allowed 5 Automation.outcomeDeclared i.actor i.resource i.id i.permission

/-- Each read is the first check of the automated request in the household
trace. -/
@[conformance_family]
def automatedRequestVisibility : Family RequestReadInput Bool where
  inputs := [⟨"user"⟩, ⟨"agent"⟩, ⟨"outsider"⟩]
  model := fun i => (automatedRequestRead i).getD false

end Conformance.Household
