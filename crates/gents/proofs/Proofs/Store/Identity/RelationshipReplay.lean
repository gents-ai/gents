import Proofs.Identity.State

namespace Identity.RelationshipReplay

/-- The native relationship owner signs the complete physical tuple and its
ordered operation. A document's application signature or DID-valued fields do
not authorize a relationship. Sequence is scoped to the exact tuple; native
storage must atomically commit sequence, tuple and document registration. -/
structure Event where
  policy : String
  resource : String
  document : String
  relation : String
  targetKind : String
  targetResource : String
  targetId : String
  targetRelation : String
  recipient : DID
  signer : DID
  sequence : Nat
  add : Bool
  deriving Repr

structure Receiver where
  policy : String
  resource : String
  document : String
  relation : String
  targetKind : String
  targetResource : String
  targetId : String
  targetRelation : String
  recipient : DID
  sequence : Nat
  granted : Bool
  deriving Repr

/-- These booleans are observations by the native receiving owner: signature
verification over every Event field, the authenticated transport recipient,
the physical document registration and current native relation-management
authority. This contract is conditional on those facts sharing the native
transaction's conflict-validated snapshot; Gents must not assert them from
application data or act as the remote signer. -/
def accepted (state : Receiver) (event : Event)
    (validSignature authenticatedTarget registered nativeManager : Bool) : Bool :=
  validSignature && authenticatedTarget && registered && nativeManager &&
    !event.signer.isEmpty && !event.targetId.isEmpty && !event.recipient.isEmpty && event.policy == state.policy &&
    event.resource == state.resource && event.document == state.document &&
    event.relation == state.relation && event.targetKind == state.targetKind &&
    event.targetResource == state.targetResource && event.targetId == state.targetId &&
    event.targetRelation == state.targetRelation && event.recipient == state.recipient &&
    event.sequence == state.sequence + 1

def receive (state : Receiver) (event : Event)
    (validSignature authenticatedTarget registered nativeManager committed : Bool) : Receiver :=
  if accepted state event validSignature authenticatedTarget registered nativeManager && committed then
    { state with sequence := event.sequence, granted := event.add }
  else state

/-- The source must sign as the actual native relationship manager. A grant
event is deliverable only if the producer transaction committed the tuple and
its event together; a DID claim alone is not a signing capability. -/
def producerMayDeliver (heldSigner nativeManager committed : Bool) : Bool :=
  heldSigner && nativeManager && committed

theorem absent_signing_capability_cannot_publish (manager committed : Bool) :
    producerMayDeliver false manager committed = false := by simp [producerMayDeliver]

theorem unverified_event_cannot_change_receiver (state : Receiver) (event : Event)
    (target registered manager committed : Bool) :
    (receive state event false target registered manager committed).granted = state.granted := by
  simp [receive, accepted]

theorem failed_receiver_commit_is_unchanged (state : Receiver) (event : Event)
    (valid target registered manager : Bool) :
    (receive state event valid target registered manager false).sequence = state.sequence := by
  simp [receive]

theorem old_grant_cannot_overwrite_later_revoke :
    let state : Receiver := ⟨"policy", "resource", "document", "reader", "actor", "", "recipient", "", "recipient", 2, false⟩
    let stale : Event := ⟨"policy", "resource", "document", "reader", "actor", "", "recipient", "", "recipient", "owner", 1, true⟩
    (receive state stale true true true true true).granted = false := by decide

theorem unrelated_recipient_is_rejected :
    let state : Receiver := ⟨"policy", "resource", "document", "reader", "actor", "", "recipient", "", "recipient", 0, false⟩
    let event : Event := ⟨"policy", "resource", "document", "reader", "actor", "", "recipient", "", "other", "owner", 1, true⟩
    (receive state event true true true true true).granted = false := by decide

theorem authenticated_grant_then_revoke :
    let state : Receiver := ⟨"policy", "resource", "document", "reader", "actor", "", "recipient", "", "recipient", 0, false⟩
    let grant : Event := ⟨"policy", "resource", "document", "reader", "actor", "", "recipient", "", "recipient", "owner", 1, true⟩
    let revoke : Event := ⟨"policy", "resource", "document", "reader", "actor", "", "recipient", "", "recipient", "owner", 2, false⟩
    let afterGrant := receive state grant true true true true true
    afterGrant.granted = true ∧ (receive afterGrant revoke true true true true true).granted = false := by decide

end Identity.RelationshipReplay
