import Proofs.Conformance.Identity.Conformance
import Proofs.Store.Identity.UnattendedReceipt
import Proofs.Conformance.Family

namespace Conformance.HouseholdC

open Lean Identity Identity.Household

deriving instance ToJson for Configuration.DesktopResetAsset
deriving instance ToJson for UnattendedReceipt.DurableState
deriving instance ToJson for UnattendedReceipt.NativeWitness
deriving instance ToJson for UnattendedReceipt.NextAction

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

structure HandoffInput where
  request : Bool
  owner : Bool
  node : Bool
  deriving ToJson

structure ReceiptInput where
  name : String
  state : UnattendedReceipt.DurableState
  actorAuthorized : Bool
  exactConsumer : Bool
  sourceVisible : Bool
  completeIndex : Bool
  authenticatedProvenance : Bool
  conflictFenced : Bool
  ownerAuthorized : Bool
  retireFenced : Bool
  deriving ToJson

structure ReceiptOutcome where
  witness : UnattendedReceipt.NativeWitness
  action : UnattendedReceipt.NextAction
  deriving ToJson

def receiptInputs : List ReceiptInput :=
  [⟨"absent", .absent, true, true, true, true, true, true, true, true⟩,
   ⟨"admitted", .admitted, true, true, true, true, true, true, true, true⟩,
   ⟨"pending_no_retirement", .pending, true, true, true, true, true, true, false, false⟩,
   ⟨"pending_fenced_retirement", .pending, true, true, true, true, true, true, true, true⟩,
   ⟨"untrusted_publisher", .admitted, true, true, true, true, false, true, true, true⟩,
   ⟨"wrong_consumer", .admitted, true, false, true, true, true, true, true, true⟩,
   ⟨"source_hidden", .admitted, true, true, false, true, true, true, true, true⟩,
   ⟨"incomplete_index", .absent, true, true, true, false, true, true, true, true⟩,
   ⟨"unfenced", .admitted, true, true, true, true, true, false, true, true⟩,
   ⟨"foreign_actor", .admitted, false, true, true, true, true, true, true, true⟩]

def receiptOutcome (i : ReceiptInput) : ReceiptOutcome :=
  let witness := UnattendedReceipt.observe i.state i.actorAuthorized i.exactConsumer
    i.sourceVisible i.completeIndex i.authenticatedProvenance i.conflictFenced
  { witness, action := UnattendedReceipt.nextAction witness i.ownerAuthorized i.retireFenced }

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
    ([.cache, .binding, .upgrade, .userKey, .userMetadata, .p2pKey, .custody, .peers] :
      List Configuration.DesktopResetAsset).map fun asset => { full, asset }
  model := fun i => Configuration.desktopResetRetires i.full i.asset

@[conformance_family]
def managedClientHandoff : Family HandoffInput Bool where
  inputs := ([false, true] : List Bool).flatMap fun request =>
    ([false, true] : List Bool).flatMap fun owner =>
      ([false, true] : List Bool).map fun node => { request, owner, node }
  model := fun i => Configuration.managedClientMayApprove i.request i.owner i.node

/-- model-production-gap: production has no native transaction-scoped receipt
witness. No production owner observes a receipt as absent, pending, admitted or
unavailable from actor authorization, exact consumer, source visibility, index
completeness, publisher provenance and a conflict fence, and no owner chooses
create, retire-pending, advance or hold from that witness; unattended delivery
reads ordinary visibility, which cannot distinguish a hidden receipt from an
absent one (`UnattendedReceipt.hidden_and_absent_are_indistinguishable`). -/
@[conformance_family]
def unattendedReceiptUnreached : Family ReceiptInput ReceiptOutcome where
  inputs := receiptInputs
  model := receiptOutcome

end Conformance.Household
