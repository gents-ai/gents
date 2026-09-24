import Proofs.Mailbox.Properties
import Proofs.Mailbox.Reply
import Proofs.ToolExecution.Executable
import Proofs.RequestExecutionLease.Properties

/-!
# Session-bound mailbox handoff

This composes existing owners for one explicit handoff. The stamped mailbox
create produces the stored row receipt first; the ordinary tool-completion
and request-terminal owners then run. It does not make all Ask/Gate notices
terminalize their producer or introduce a waiting request state.

An end-to-end native refinement would have to show that the owned loop observes
the committed `file_mailbox_item` tool result before terminalizing. The current
native regression checks the real storage receipt and explicit terminal owner
sequence, but does not exercise an AgentToolCall row or the completion loop.
This model does not treat a provider assertion or Boolean as a stored row.
-/
namespace Mailbox.Handoff

structure Producer where
  requestId : RequestId
  requestDocId : String
  sessionId : String
  requesterDid : String
  agentDid : String
  lease : RequestExecutionLease.World Nat
  tool : ToolExecution.ToolCallContext
  deriving DecidableEq

structure Receipt where
  registry : RegistryState
  question : StoredEnvelope
  producer : Producer
  deriving DecidableEq

def handoffKind : Kind → Bool
  | .ask | .gate => true
  | _ => false

/-- A matching open-row retry may reuse its stored row, but a
condition-coalesced row from another origin or session cannot satisfy the
exact receipt check. -/
def eligible (producer : Producer) (request : CreateRequest) : Bool :=
  handoffKind request.identity.kind && request.handling == .startRequest &&
      producer.tool.awaitMode == .foreground &&
      request.sessionId != "" && request.requestId != 0 &&
      producer.sessionId != "" && producer.requestId != 0 &&
      producer.requestDocId != "" &&
      producer.requesterDid != "" && producer.agentDid != "" &&
      request.sessionId == producer.sessionId &&
      request.requestId == producer.requestId &&
      request.identity.requesterDid == producer.requesterDid &&
      request.identity.agentDid == producer.agentDid &&
      producer.tool.requestId == producer.requestId

/-- An explicit, session-bound handoff only. The three calls are the existing
stamped mailbox, tool lifecycle, and request lease owners, in that order. -/
def fileAndComplete? (registry : RegistryState) (producer : Producer)
    (request : CreateRequest) (generation : Nat) : Option Receipt :=
  if !eligible producer request then none else
  match storedCreateReceipt? registry request with
  | none => none
  | some (stored, question) =>
      match ToolExecution.ToolCallContext.step? producer.tool .complete with
      | none => none
      | some completedTool =>
          match RequestExecutionLease.step? producer.lease
              (.finalize .mutationWriteGate generation .completed) with
          | none => none
          | some completedLease => some {
              registry := stored
              question := question
              producer := { producer with lease := completedLease, tool := completedTool }
            }

theorem successful_handoff_has_stored_question_and_terminal_producer
    (registry : RegistryState) (producer : Producer) (request : CreateRequest)
    (generation : Nat) (receipt : Receipt)
    (h : fileAndComplete? registry producer request generation = some receipt) :
    (∃ stored ∈ receipt.registry.rows,
      stored.envelope = receipt.question ∧ stored.isOpen = true) ∧
      receipt.producer.lease.request = .completed ∧
      receipt.producer.tool.state = .completed ∧
      receipt.question.sessionId = producer.sessionId ∧
      receipt.question.requestId = producer.requestId ∧
      receipt.question.identity.requesterDid = producer.requesterDid ∧
      receipt.question.identity.agentDid = producer.agentDid := by
  by_cases he : eligible producer request = true
  · unfold fileAndComplete? at h
    simp only [he, Bool.not_true, Bool.false_eq_true, ↓reduceIte] at h
    cases hfile : storedCreateReceipt? registry request with
    | none => simp [hfile] at h
    | some filed =>
        rcases filed with ⟨stored, question⟩
        cases htool : ToolExecution.ToolCallContext.step? producer.tool .complete with
        | none => simp [hfile, htool] at h
        | some completedTool =>
            cases hlease : RequestExecutionLease.step? producer.lease
                (.finalize .mutationWriteGate generation .completed) with
            | none => simp [hfile, htool, hlease] at h
            | some completedLease =>
                simp [hfile, htool, hlease] at h
                cases h
                have hstored := stored_receipt_is_durable registry stored request question hfile
                have hterminal := RequestExecutionLease.terminalization_agrees_atomically
                  producer.lease completedLease generation .completed hlease
                have htoolState : completedTool.state = .completed := by
                  simp [ToolExecution.ToolCallContext.step?] at htool
                  rw [← htool.2]
                have hbinding : request.sessionId = producer.sessionId ∧
                    request.requestId = producer.requestId ∧
                    request.identity.requesterDid = producer.requesterDid ∧
                    request.identity.agentDid = producer.agentDid := by
                  simp [eligible] at he
                  aesop
                exact ⟨hstored.1, hterminal.2, htoolState,
                  hstored.2.2.2.2.2.1.trans hbinding.1,
                  hstored.2.2.2.2.2.2.1.trans hbinding.2.1,
                  hstored.2.2.1.trans hbinding.2.2.1,
                  hstored.2.2.2.1.trans hbinding.2.2.2⟩
  · have hf : eligible producer request = false := Bool.eq_false_iff.mpr he
    simp [fileAndComplete?, hf] at h

/-- The reply is a new signed request. Its source is the physical mailbox row,
and the stored handoff session is mandatory rather than an optional route. The
existing reply claim owner still decides authentication, tenancy, deadline,
terminal idempotence, and one-request consumption. -/
def claimLinkedReply? (receipt : Receipt) (item : Item)
    (envelope : ReplyEnvelope) (reply : ReplyEvidence) : Option Item := do
  if item.identity != receipt.question.identity ||
      envelope.docId != receipt.question.docId ||
      envelope.targetAgentDid != receipt.question.identity.agentDid ||
      envelope.sessionId != some receipt.question.sessionId ||
      receipt.question.sessionId == "" ||
      reply.requestDocId == receipt.producer.requestDocId then none
  claimReply? item envelope reply

theorem successful_linked_reply_has_original_session_and_new_request
    (receipt : Receipt) (item post : Item) (envelope : ReplyEnvelope)
    (reply : ReplyEvidence)
    (h : claimLinkedReply? receipt item envelope reply = some post) :
    envelope.sessionId = some receipt.question.sessionId ∧
      reply.sessionId = receipt.question.sessionId ∧
      reply.sourceDocId = receipt.question.docId ∧
      reply.requesterDid = receipt.question.identity.requesterDid ∧
      reply.agentDid = receipt.question.identity.agentDid ∧
      reply.requestDocId ≠ receipt.producer.requestDocId := by
  unfold claimLinkedReply? at h
  split at h
  · simp at h
  · rename_i hguard
    have hadmit := reply_claim_requires_admission item post envelope reply h
    simp at hguard
    unfold replyMatches at hadmit
    aesop

def question (key doc : String) (request : RequestId)
    (session content : String) : CreateRequest :=
  { identity :=
      { itemKey := key, requesterDid := "owner", agentDid := "agent",
        sourceKind := .agent, sourceId := "condition-key", kind := .ask }
    context := ⟨"owner", "agent"⟩
    docId := doc
    handling := .startRequest
    sessionId := session
    requestId := request
    content := content }

def producer : Producer :=
  { requestId := 1, requestDocId := "parent-doc", sessionId := "session"
    requesterDid := "owner", agentDid := "agent"
    lease := { (RequestExecutionLease.initial Nat) with
      request := .processing, lease := .active 7 10 10,
      usedGenerations := [7], now := 5 }
    tool :=
      { callId := 11, requestId := 1, state := .running,
        operation := .nativeCommand, deadline := 10, currentTime := 5,
        persistence := .committed } }

def openQuestion : CreateRequest :=
  question "ask:1" "mailbox-doc" 1 "session" "What should I do?"

theorem filed_question_precedes_owned_completion :
    (fileAndComplete? {} producer openQuestion 7).isSome = true := by
  native_decide

theorem failed_filing_cannot_complete_handoff :
    (fileAndComplete? {} producer
      { openQuestion with context := ⟨"foreign", "agent"⟩ } 7) = none := by
  native_decide

/-- An old terminal condition row cannot masquerade as the current open
question, even though both share the same owner/source prefix. -/
theorem terminal_old_question_cannot_satisfy_current_handoff :
    let old := applyCreate {} (question "ask:1" "old-doc" 2 "old-session" "old?")
    let closed := terminalizePrefix old openQuestion.identity.ownerPrefix
    let current := applyCreate closed (question "ask:2" "new-doc" 1 "session" "new?")
    (storedCreateReceipt? current
      (question "ask:3" "retry-doc" 2 "old-session" "old?")).isNone = true := by
  native_decide

theorem foreign_agent_cannot_reuse_open_question :
    let stored := applyCreate {} openQuestion
    let foreign := { openQuestion with
      identity := { openQuestion.identity with agentDid := "other-agent" },
      context := ⟨"owner", "other-agent"⟩ }
    (storedCreateReceipt? stored foreign).isNone = true := by
  native_decide

def replyEnvelope : ReplyEnvelope :=
  { docId := "mailbox-doc", handling := .startRequest,
    targetAgentDid := "agent", behaviorId := "repair",
    sessionId := some "session", deadlineValid := true }

def replyEvidence : ReplyEvidence :=
  { requestDocId := "reply-doc", sourceDocId := "mailbox-doc",
    requesterDid := "owner", agentDid := "agent", behaviorId := "repair",
    sessionId := "session", authenticated := true, interactive := true }

theorem linked_reply_is_a_new_request_in_original_session :
    (match fileAndComplete? {} producer openQuestion 7 with
     | none => false
     | some receipt =>
         let item : Item := ⟨receipt.question.identity, .open, ""⟩
         (claimLinkedReply? receipt item replyEnvelope replyEvidence).isSome &&
           (claimLinkedReply? receipt item
             { replyEnvelope with sessionId := some "other-session" } replyEvidence).isNone &&
           (claimLinkedReply? receipt item replyEnvelope
             { replyEvidence with requestDocId := "parent-doc" }).isNone) = true := by
  native_decide

end Mailbox.Handoff
