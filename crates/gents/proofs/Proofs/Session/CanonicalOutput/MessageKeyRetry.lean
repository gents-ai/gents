import Proofs.Shared.Basic

namespace CanonicalOutput.MessageKeyRetry

/-- The immutable fact bound to one `AgentMessage.message_key` within a session
scope: its request, execution generation, source identity, and payload digest. -/
structure MessageFact where
  messageKey : String
  requestDocId : Nat
  executionGeneration : Nat
  sourceId : Nat
  payloadDigest : Nat
  deriving DecidableEq, Repr

/-- Decision when publishing an `AgentMessage` under one `message_key`:
- no visible row under `message_key` -> create a fresh immutable row;
- one visible row matching the exact request, generation, source, and payload ->
  idempotent replay returning the existing document;
- one visible row disagreeing on any field, or multiple twin rows under the same
  `message_key` -> fail closed as a typed integrity conflict. -/
inductive MessageWriteOutcome where
  | create
  | idempotentReplay
  | integrityConflict
  deriving DecidableEq, Repr

namespace MessageWriteOutcome

def toContract : MessageWriteOutcome → String
  | .create => "create"
  | .idempotentReplay => "idempotent_replay"
  | .integrityConflict => "integrity_conflict"

end MessageWriteOutcome

def decideMessageWrite (existingUnderKey : List MessageFact) (candidate : MessageFact) :
    MessageWriteOutcome :=
  match existingUnderKey with
  | [] => .create
  | [stored] => if stored == candidate then .idempotentReplay else .integrityConflict
  | _ :: _ :: _ => .integrityConflict

theorem fresh_key_creates (candidate : MessageFact) :
    decideMessageWrite [] candidate = .create := rfl

theorem exact_retry_is_idempotent (fact : MessageFact) :
    decideMessageWrite [fact] fact = .idempotentReplay := by
  simp [decideMessageWrite]

theorem mismatched_retry_is_integrity_conflict (stored candidate : MessageFact)
    (hdiff : stored ≠ candidate) :
    decideMessageWrite [stored] candidate = .integrityConflict := by
  simp [decideMessageWrite, beq_eq_false_iff_ne, hdiff]

theorem mismatched_source_retry_is_integrity_conflict (stored candidate : MessageFact)
    (hsource : stored.sourceId ≠ candidate.sourceId) :
    decideMessageWrite [stored] candidate = .integrityConflict := by
  apply mismatched_retry_is_integrity_conflict
  intro heq
  exact hsource (congrArg MessageFact.sourceId heq)

theorem mismatched_payload_retry_is_integrity_conflict (stored candidate : MessageFact)
    (hpayload : stored.payloadDigest ≠ candidate.payloadDigest) :
    decideMessageWrite [stored] candidate = .integrityConflict := by
  apply mismatched_retry_is_integrity_conflict
  intro heq
  exact hpayload (congrArg MessageFact.payloadDigest heq)

theorem duplicate_key_twins_are_integrity_conflict
    (first second : MessageFact) (rest : List MessageFact) (candidate : MessageFact) :
    decideMessageWrite (first :: second :: rest) candidate = .integrityConflict := rfl

theorem existing_key_never_recreates (existing : List MessageFact) (candidate : MessageFact)
    (hnonempty : existing ≠ []) :
    decideMessageWrite existing candidate ≠ .create := by
  cases existing with
  | nil => contradiction
  | cons head tail =>
      cases tail with
      | nil =>
          simp only [decideMessageWrite]
          split <;> simp
      | cons _ _ =>
          simp [decideMessageWrite]

end CanonicalOutput.MessageKeyRetry
