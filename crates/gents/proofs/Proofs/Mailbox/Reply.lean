import Proofs.Mailbox.State

namespace Mailbox

structure ReplyEvidence where
  requestDocId : String
  sourceDocId : String
  requesterDid : String
  nodeDid : String
  agentId : String
  sessionId : String
  authenticated : Bool
  interactive : Bool
  deriving DecidableEq, Repr

structure ReplyEnvelope where
  docId : String
  handling : Handling
  targetNodeDid : String
  agentId : String
  sessionId : Option String
  deadlineValid : Bool
  questionCurrent : Bool := true
  deriving DecidableEq, Repr

def replyMatches (item : Item) (envelope : ReplyEnvelope) (reply : ReplyEvidence) : Prop :=
  envelope.handling = .startRequest ∧ reply.authenticated = true ∧ reply.interactive = true ∧
  (envelope.questionCurrent = true ∨
    (item.status = .acted ∧ item.resolvedDocId = reply.requestDocId)) ∧
  (envelope.deadlineValid = true ∨
    (item.status = .acted ∧ item.resolvedDocId = reply.requestDocId)) ∧
  reply.requestDocId ≠ "" ∧ reply.requesterDid ≠ "" ∧
  reply.requesterDid = item.identity.requesterDid ∧ reply.nodeDid = envelope.targetNodeDid ∧
  envelope.agentId ≠ "" ∧ reply.agentId = envelope.agentId ∧
  (envelope.sessionId = none ∨ envelope.sessionId = some reply.sessionId) ∧
  envelope.docId ≠ "" ∧ reply.sourceDocId = envelope.docId ∧ reply.nodeDid ≠ ""

instance (item : Item) (envelope : ReplyEnvelope) (reply : ReplyEvidence) :
    Decidable (replyMatches item envelope reply) := by
  unfold replyMatches
  infer_instance

def claimReply? (item : Item) (envelope : ReplyEnvelope) (reply : ReplyEvidence) : Option Item :=
  if replyMatches item envelope reply then
    match item.status with
    | .open => some { item with status := .acted, resolvedDocId := reply.requestDocId }
    | .acted => if item.resolvedDocId = reply.requestDocId then some item else none
    | .dismissed | .expired => none
  else none

theorem reply_claim_requires_admission (item post : Item) (envelope : ReplyEnvelope)
    (reply : ReplyEvidence)
    (h : claimReply? item envelope reply = some post) :
    replyMatches item envelope reply := by
  unfold claimReply? at h
  split at h
  · assumption
  · simp at h

theorem open_reply_claim_records_receipt (item : Item) (envelope : ReplyEnvelope)
    (reply : ReplyEvidence) (hopen : item.status = .open)
    (hmatch : replyMatches item envelope reply) :
    claimReply? item envelope reply =
      some { item with status := .acted, resolvedDocId := reply.requestDocId } := by
  simp [claimReply?, hmatch, hopen]

theorem same_reply_claim_is_idempotent (item : Item) (envelope : ReplyEnvelope)
    (reply : ReplyEvidence) (hacted : item.status = .acted)
    (hreceipt : item.resolvedDocId = reply.requestDocId)
    (hmatch : replyMatches item envelope reply) :
    claimReply? item envelope reply = some item := by
  simp [claimReply?, hmatch, hacted, hreceipt]

theorem dismissed_reply_is_rejected (item : Item) (envelope : ReplyEnvelope)
    (reply : ReplyEvidence) (h : item.status = .dismissed ∨ item.status = .expired) :
    claimReply? item envelope reply = none := by
  rcases h with h | h <;> simp [claimReply?, h]

theorem consumed_reply_rejects_another_request (item : Item) (envelope : ReplyEnvelope)
    (reply : ReplyEvidence) (hacted : item.status = .acted)
    (hdifferent : item.resolvedDocId ≠ reply.requestDocId) :
    claimReply? item envelope reply = none := by
  simp [claimReply?, hacted, hdifferent]

theorem reply_claim_preserves_identity (item post : Item) (envelope : ReplyEnvelope)
    (reply : ReplyEvidence)
    (h : claimReply? item envelope reply = some post) :
    post.identity = item.identity := by
  unfold claimReply? at h
  split at h
  · cases hs : item.status <;> simp [hs] at h
    · cases h; rfl
    · rcases h with ⟨_, rfl⟩
      rfl
  · simp at h

/-- Confirmation is a persisted, verified caller receipt. The observation is a
native read of the exact item; filtered-list absence supplies no observation.
Acknowledgement must complete before the local answer cache is retired. -/
structure AnswerCacheBinding where
  client : String
  node : String
  agent : String
  session : String
  item : String
  sourceItem : String
  confirmed : Bool
  deriving DecidableEq, Repr

structure AnswerCacheObservation where
  requester : String
  node : String
  agent : String
  session : String
  item : String
  kind : Kind
  handling : Handling
  status : Status
  deriving DecidableEq, Repr

def mayRetireAnswerCache (binding : AnswerCacheBinding)
    (observation : Option AnswerCacheObservation) (currentClient : String)
    (acknowledged : Bool) : Bool :=
  binding.confirmed && acknowledged && currentClient == binding.client &&
    binding.sourceItem == binding.item &&
    [binding.client, binding.node, binding.agent, binding.session, binding.item].all (· != "") &&
    match observation with
    | none => false
    | some observed =>
        observed.requester == binding.client && observed.node == binding.node &&
        observed.agent == binding.agent && observed.session == binding.session &&
        observed.item == binding.item && observed.kind == .ask &&
        observed.handling == .startRequest && observed.status.terminal

theorem retirement_requires_confirmed_acknowledgement
    (binding : AnswerCacheBinding) (observation : Option AnswerCacheObservation)
    (currentClient : String) (acknowledged : Bool)
    (h : mayRetireAnswerCache binding observation currentClient acknowledged = true) :
    binding.confirmed = true ∧ acknowledged = true := by
  simp only [mayRetireAnswerCache, Bool.and_eq_true] at h
  aesop

theorem missing_item_cannot_retire_answer (binding : AnswerCacheBinding)
    (currentClient : String) (acknowledged : Bool) :
    mayRetireAnswerCache binding none currentClient acknowledged = false := by
  simp [mayRetireAnswerCache]


end Mailbox
