import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family

namespace Conformance.HouseholdC

open Lean Identity Identity.Household

deriving instance ToJson for Configuration.DesktopResetAsset

structure TransactionActorInput where
  actor : Option String
  requester : String
  deriving ToJson

def transactionActorInputs : List TransactionActorInput :=
  [⟨some "user", "user"⟩, ⟨some "agent", "user"⟩, ⟨none, "user"⟩, ⟨some "", ""⟩]

structure GraphHandoffInput where
  actor : Bool
  command : Bool
  create : Bool
  authorized : Bool
  replacing : Bool
  delete : Bool
  deriving ToJson

structure ResetInput where
  full : Bool
  asset : Configuration.DesktopResetAsset
  deriving ToJson

end Conformance.HouseholdC

namespace Conformance.Household

open Lean Identity Identity.Household Conformance.HouseholdC

@[conformance_family]
def clientGoalTransactionActor : Family TransactionActorInput Bool where
  inputs := transactionActorInputs
  model := fun i => Configuration.clientGoalTransactionActorMatches i.actor i.requester

/-- The staging scenario signs the request as the User and stages it in the
managed Agent's own transaction without a client graph handoff, so it builds
only the Agent-for-User case, once fresh and once replacing a terminal Goal. -/
@[conformance_family]
def clientGoalActorFence : Family TransactionActorInput Bool where
  inputs := transactionActorInputs.filter fun i => i.actor == some "agent" && i.requester == "user"
  model := fun i => Configuration.clientGoalTransactionActorMatches i.actor i.requester

@[conformance_family]
def clientGoalGraphHandoff : Family GraphHandoffInput Bool where
  inputs := [⟨true, true, true, true, false, false⟩,
    ⟨true, true, true, true, true, true⟩, ⟨false, true, true, true, true, true⟩,
    ⟨true, false, true, true, true, true⟩, ⟨true, true, false, true, true, true⟩,
    ⟨true, true, true, false, true, true⟩, ⟨true, true, true, true, true, false⟩]
  model := fun i => Configuration.clientGoalGraphHandoffAllowed i.actor i.command i.create
    i.authorized i.replacing i.delete

@[conformance_family]
def desktopReset : Family ResetInput Bool where
  inputs := ([false, true] : List Bool).flatMap fun full =>
    ([.cache, .binding, .upgrade, .userKey, .userMetadata, .custody, .connections] :
      List Configuration.DesktopResetAsset).map fun asset => { full, asset }
  model := fun i => Configuration.desktopResetRetires i.full i.asset

end Conformance.Household
