import Proofs.Mailbox.State

namespace Mailbox

inductive ResolutionAction where
  | act (resolvedDocId : String)
  | dismiss (principalDid : String)
  | expire (deadlineDue : Bool)
  deriving DecidableEq, Repr

namespace ResolutionAction

def toContract : ResolutionAction → String
  | .act _ => "act"
  | .dismiss _ => "dismiss"
  | .expire _ => "expire"

end ResolutionAction

/-- Status-only transition used by the executable state-machine contract. -/
def stepStatus? (status : Status) : ResolutionAction → Option Status
  | .act _ => if status = .open then some .acted else none
  | .dismiss _ => if status = .open then some .dismissed else none
  | .expire due => if status = .open ∧ due then some .expired else none

/-- Row transition.  It additionally enforces non-empty satisfying document
for `acted` and requester identity for `dismissed`. -/
def applyResolution? (item : Item) : ResolutionAction → Option Item
  | .act resolvedDocId =>
      if item.status = .open ∧ resolvedDocId ≠ "" then
        some { item with status := .acted, resolvedDocId := resolvedDocId }
      else none
  | .dismiss principalDid =>
      if item.status = .open ∧ principalDid = item.identity.requesterDid then
        some { item with status := .dismissed, resolvedDocId := "" }
      else none
  | .expire due =>
      if item.status = .open ∧ due then
        some { item with status := .expired, resolvedDocId := "" }
      else none

def stamped (request : CreateRequest) : Bool :=
  request.identity.requesterDid = request.context.requesterDid &&
    request.identity.agentDid = request.context.agentDid &&
    request.identity.requesterDid != "" &&
    request.identity.agentDid != "" &&
    request.identity.itemKey != "" &&
    request.identity.sourceId != ""

/-- Stamped create.  An in-flight retry for an owner-matching open prefix is
the identity operation.  A terminal re-ask has no open prefix and may add a
fresh item key.  A reused key fails closed as another identity operation. -/
def applyCreate (state : RegistryState) (request : CreateRequest) : RegistryState :=
  if !stamped request then state
  else if request.identity.ownerPrefix ∈ state.openPrefixes then state
  else if request.identity.itemKey ∈ state.itemKeys then state
  else
    { state with
      openPrefixes := insert request.identity.ownerPrefix state.openPrefixes
      itemKeys := insert request.identity.itemKey state.itemKeys }

/-- Terminalization removes the open prefix but never removes the durable key,
which allows the next occurrence to mint a new key without reopening. -/
def terminalizePrefix (state : RegistryState) (ownerPrefix : OwnerPrefix) : RegistryState :=
  { state with openPrefixes := state.openPrefixes.erase ownerPrefix }

end Mailbox
