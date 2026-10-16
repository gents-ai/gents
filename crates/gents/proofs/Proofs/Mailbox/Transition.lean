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

def stepStatus? (status : Status) : ResolutionAction → Option Status
  | .act _ => if status = .open then some .acted else none
  | .dismiss _ => if status = .open then some .dismissed else none
  | .expire due => if status = .open ∧ due then some .expired else none

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
    request.identity.nodeDid = request.context.nodeDid &&
    request.identity.requesterDid != "" &&
    request.identity.nodeDid != "" &&
    request.identity.itemKey != "" &&
    request.identity.sourceId != "" &&
    request.docId != ""

def applyCreate (state : RegistryState) (request : CreateRequest) : RegistryState :=
  if !stamped request then state
  else if request.identity.ownerPrefix ∈ state.openPrefixes then state
  else if request.identity.itemKey ∈ state.itemKeys then state
  else
    { state with rows :=
        ⟨request.storedEnvelope, true⟩ :: state.rows }

def storedCreateReceipt? (state : RegistryState) (request : CreateRequest) :
    Option (RegistryState × StoredEnvelope) :=
  if !stamped request then none else
  let post := applyCreate state request
  match post.rows.find? (fun row =>
      row.isOpen &&
      row.envelope.identity.ownerPrefix == request.identity.ownerPrefix &&
      row.envelope.identity.nodeDid == request.identity.nodeDid &&
      row.envelope.handling == request.handling &&
      row.envelope.sessionId == request.sessionId &&
      row.envelope.requestId == request.requestId &&
      row.envelope.content == request.content) with
  | none => none
  | some row => some (post, row.envelope)

def terminalizePrefix (state : RegistryState) (ownerPrefix : OwnerPrefix) : RegistryState :=
  { state with rows := state.rows.map fun row =>
      if row.envelope.identity.ownerPrefix == ownerPrefix then
        { row with isOpen := false }
      else row }

end Mailbox
