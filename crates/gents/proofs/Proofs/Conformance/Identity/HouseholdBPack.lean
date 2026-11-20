import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Identity.HouseholdStore
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household Identity.Conformance

structure PackCleanupInput where
  name : String
  visible : List Bool
  deriving ToJson

@[conformance_family]
def packCleanupVisible : Family PackCleanupInput Bool where
  inputs := [("none", []), ("all-visible", [true, true]), ("unavailable-first", [false, true]),
    ("unavailable-later", [true, false])].map fun (name, visible) => { name, visible }
  model := fun i => Configuration.packCleanupInputsVisible i.visible

example : Configuration.packCleanupInputsVisible [false, true] = false := by decide

structure PackRemovalInput where
  name : String
  actor : String
  role : String
  writer : Bool
  returnedMatches : Bool
  deriving ToJson

def packRemovalState (writer : Bool) : State :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩], enrolled := [] }
  let published := Configuration.publishPackInstallation initial agentDid "pack" "home" true true
  if writer then apply published
    (.grantAs agentDid ⟨"configuration", "pack", "writer", .actor siblingDid⟩) else published

def packRemovalCases : List PackRemovalInput :=
  [("agent-owner", agentDid, false, true), ("user-parent", userDid, false, true),
   ("runtime-reader", runtimeDid, false, true), ("agent-writer", siblingDid, true, true),
   ("wrong-document", agentDid, false, false)].map fun (name, actor, writer, returnedMatches) =>
    { name, actor, role := roleOf actor, writer, returnedMatches }

def packRemovalModel (i : PackRemovalInput) : Bool :=
  Configuration.packRemovalConfirmed (packRemovalState i.writer) i.actor "pack"
    (if i.returnedMatches then "pack" else "other")

/-- The delete confirmation reads only the returned physical identity, so it is
replayed for the cases whose actor holds delete authority over the record: the
matching and the wrong document. The other actors' authority is observed by
native ACP in `packRemovalDeleteAccess`. -/
@[conformance_family]
def packRemovalConfirmation : Family PackRemovalInput Bool where
  inputs := packRemovalCases.filter fun i => i.name ∈ ["agent-owner", "wrong-document"]
  model := packRemovalModel

/-- A served node returns the deleted document's own identity, so native delete
authority is observed per actor only for the cases whose returned identity matches. -/
@[conformance_family]
def packRemovalDeleteAccess : Family PackRemovalInput Bool where
  inputs := packRemovalCases.filter fun i => i.returnedMatches
  model := packRemovalModel

example : packRemovalModel ⟨"agent-owner", agentDid, "agent", false, true⟩ = true := by native_decide

structure PackInstallationInput where
  name : String
  installer : String
  commit : Bool
  revokeParent : Bool
  retryFresh : Bool
  direct : Bool
  deriving ToJson

def packInstallationState (i : PackInstallationInput) : State :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩], enrolled := [] }
  let published := if i.direct then directCreate initial i.installer "configuration" "pack" true true
    else Configuration.publishPackInstallation initial i.installer "pack" "home" true i.commit
  let state := if i.revokeParent then apply published
    (.revoke ⟨"configuration", "pack", "parent", .object "nodes" "home"⟩) else published
  Configuration.publishPackInstallation state i.installer "pack" "home" i.retryFresh true

def packInstallationCases : List PackInstallationInput :=
  [("user-created", userDid, true, false, false, false),
   ("agent-created", agentDid, true, false, false, false),
   ("agent-parent-revoked", agentDid, true, true, false, false),
   ("agent-parent-revoked-claimed-fresh", agentDid, true, true, true, false),
   ("aborted", agentDid, false, false, false, false),
   ("direct-orphan", agentDid, true, false, false, true)].map
    fun (name, installer, commit, revokeParent, retryFresh, direct) =>
      { name, installer, commit, revokeParent, retryFresh, direct }

structure PackInstallationAccess where
  installerRead : Bool
  installerUpdate : Bool
  userRead : Bool
  userUpdate : Bool
  userDelete : Bool
  runtimeRead : Bool
  otherRead : Bool
  deriving ToJson

@[conformance_family]
def packInstallationAccess : Family PackInstallationInput PackInstallationAccess where
  inputs := packInstallationCases.filter fun i => i.commit
  model := fun i =>
    let allowed := fun actor permission =>
      Household.allowed 5 (packInstallationState i) actor "configuration" "pack" permission
    { installerRead := allowed i.installer "read"
      installerUpdate := allowed i.installer "update"
      userRead := allowed userDid "read"
      userUpdate := allowed userDid "update"
      userDelete := allowed userDid "delete"
      runtimeRead := allowed runtimeDid "read"
      otherRead := allowed siblingDid "read" }

structure PackInstallationAbortedAccess where
  installerRead : Bool
  userRead : Bool
  deriving ToJson

/-- An aborted publication leaves no document, so a served node can observe only
whether the installer and the User read a record; per-permission access has no
document to be asked about. -/
@[conformance_family]
def packInstallationAborted : Family PackInstallationInput PackInstallationAbortedAccess where
  inputs := packInstallationCases.filter fun i => !i.commit
  model := fun i =>
    let allowed := fun actor =>
      Household.allowed 5 (packInstallationState i) actor "configuration" "pack" "read"
    { installerRead := allowed i.installer, userRead := allowed userDid }

end Conformance.Household
