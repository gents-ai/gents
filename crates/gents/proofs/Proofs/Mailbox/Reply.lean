import Proofs.Mailbox.State

namespace Mailbox

/-- Facts from the existing signed request admission and immutable request row.
This model consumes a reply, not a grant to execute arbitrary host commands. -/
structure ReplyEvidence where
  requestDocId : String
  sourceDocId : String
  requesterDid : String
  agentDid : String
  behaviorId : String
  sessionId : String
  authenticated : Bool
  interactive : Bool
  deriving DecidableEq, Repr

structure ReplyEnvelope where
  docId : String
  handling : Handling
  targetAgentDid : String
  behaviorId : String
  sessionId : Option String
  deadlineValid : Bool
  deriving DecidableEq, Repr

def replyMatches (item : Item) (envelope : ReplyEnvelope) (reply : ReplyEvidence) : Prop :=
  envelope.handling = .startRequest ∧ reply.authenticated = true ∧ reply.interactive = true ∧
  (envelope.deadlineValid = true ∨
    (item.status = .acted ∧ item.resolvedDocId = reply.requestDocId)) ∧
  reply.requestDocId ≠ "" ∧ reply.requesterDid ≠ "" ∧
  reply.requesterDid = item.identity.requesterDid ∧ reply.agentDid = envelope.targetAgentDid ∧
  envelope.behaviorId ≠ "" ∧ reply.behaviorId = envelope.behaviorId ∧
  (envelope.sessionId = none ∨ envelope.sessionId = some reply.sessionId) ∧
  envelope.docId ≠ "" ∧ reply.sourceDocId = envelope.docId ∧ reply.agentDid ≠ ""

instance (item : Item) (envelope : ReplyEnvelope) (reply : ReplyEvidence) :
    Decidable (replyMatches item envelope reply) := by
  unfold replyMatches
  infer_instance

/-- Consumption belongs in the existing request-claim transaction. An already
consumed item permits only the same request; its lifecycle owner still decides
whether that request may resume. Dismissal/expiry never admits a reply. -/
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

end Mailbox
