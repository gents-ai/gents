import Lean
import Proofs.Identity.Automation
import Proofs.Identity.DeclaredAccess

namespace Identity.PackPlacementReaders

open Identity.Household

/-- A RepositoryPlacement's `readers` is pack-declared access policy
(`DeclaredAccess`): its callback workspace effects read the placement as the
selected Agent (`Automation.callbackPlacementReadable`), and only the Agents
the pack declares there may. The installing User publishes the placement and
applies the declared tuples in the same configuration transaction; removing the
placement, as uninstall does, revokes them first. -/
def install (state : State) (installer : String) (prior declared : List String) : State :=
  DeclaredAccess.applyDeclared state installer "configuration" "placement" prior declared

def uninstall (state : State) (installer : String) (declared : List String) : State :=
  DeclaredAccess.revokeAll state installer "configuration" "placement" declared

/-- The User publishes the placement under its Node; the pack provisions three
slot Agents and declares two of them as placement readers. -/
def fixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "placement", "user"⟩],
    relations := [⟨"configuration", "placement", "parent", .object "nodes" "node"⟩],
    enrolled := [] }

def provisioned : List String := ["writer", "reviewer", "integrator"]
def declared : List String := ["writer", "reviewer"]

def installed : State := install fixture "user" [] declared
def reinstalledAfterRevoke : State :=
  install (apply installed (.revoke (DeclaredAccess.readerRelation "configuration" "placement" "reviewer")))
    "user" declared declared
def revokedBeforeRemoval : State := uninstall installed "user" declared
/-- Uninstall then removes the placement the pack created. -/
def removed : State :=
  { revokedBeforeRemoval with objects := revokedBeforeRemoval.objects.filter fun object =>
      !(object.resource == "configuration" && object.id == "placement") }
def foreignInstall : State := install fixture "other" [] declared

def readable (state : State) (actor : String) : Bool :=
  Automation.callbackPlacementReadable state actor "placement"

theorem declaration_is_admissible : DeclaredAccess.admissible provisioned "user" declared = true := by
  native_decide

theorem effective_readers_are_exactly_the_declared :
    ["writer", "reviewer", "integrator", "other", "runtime"].map (readable installed) =
      [true, true, false, false, false] := by native_decide

theorem undeclared_slot_read_stays_denied : readable installed "integrator" = false := by
  native_decide

/-- The declared tuples are revoked before the placement is removed, so a
recreated placement (whose content-addressed identity may repeat) does not
revive them. -/
theorem uninstall_revokes_every_declared_reader :
    ["writer", "reviewer", "integrator", "other"].map (readable revokedBeforeRemoval) =
      [false, false, false, false] ∧
    readable revokedBeforeRemoval "user" = true ∧
    ["user", "writer", "reviewer", "integrator", "other"].map (readable removed) =
      [false, false, false, false, false] := by native_decide

theorem reinstall_does_not_restore_revoked_reader :
    ["writer", "reviewer", "integrator"].map (readable reinstalledAfterRevoke) =
      [true, false, false] := by native_decide

/-- A grantor that does not manage the placement applies nothing: grants never
exceed the installing actor's authority. -/
theorem grants_never_exceed_the_installer :
    foreignInstall.relations.length = fixture.relations.length ∧
    ["writer", "reviewer"].map (readable foreignInstall) = [false, false] := by native_decide

def casesJson : String :=
  let states : List (String × State) :=
    [("installed", installed), ("reinstalled_after_revoke", reinstalledAfterRevoke),
     ("removed", removed)]
  (Lean.toJson (states.map fun (name, state) => Lean.Json.mkObj
    [("state", Lean.toJson name), ("declared", Lean.toJson declared),
     ("readers", Lean.toJson (["user", "writer", "reviewer", "integrator", "other"].map fun actor =>
        Lean.Json.mkObj [("actor", Lean.toJson actor), ("read", Lean.toJson (readable state actor))]))])).compress

end Identity.PackPlacementReaders
