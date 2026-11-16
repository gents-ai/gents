import Proofs.Automation.Mailbox.Handoff
import Proofs.Conformance.Family

namespace Conformance.MailboxHandoffContracts
open Mailbox Mailbox.Handoff

private structure Case where
  name : String
  registry : RegistryState := {}
  producer : Producer := Handoff.producer
  question : CreateRequest := openQuestion
  envelope : ReplyEnvelope := replyEnvelope
  reply : ReplyEvidence := replyEvidence

private def staleRegistry : RegistryState :=
  let old := applyCreate {} (question "ask:1" "old-doc" 2 "old-session" "old?")
  let closed := terminalizePrefix old openQuestion.identity.ownerPrefix
  applyCreate closed (question "ask:2" "new-doc" 1 "session" "new?")

private def cases : List Case :=
  [ { name := "fresh-linked" }
  , { name := "request-mismatch", question := { openQuestion with requestId := 2 } }
  , { name := "session-mismatch", question := { openQuestion with sessionId := "other" } }
  , { name := "tool-owner-mismatch", producer :=
        { Handoff.producer with tool := { Handoff.producer.tool with requestId := 2 } } }
  , { name := "producer-agent-mismatch", producer :=
        { Handoff.producer with nodeDid := "other-node" } }
  , { name := "producer-requester-mismatch", producer :=
        { Handoff.producer with requesterDid := "other-requester" } }
  , { name := "unstamped", question :=
        { openQuestion with context := ⟨"foreign", "node"⟩ } }
  , { name := "terminal-old-row", registry := staleRegistry,
      producer := { Handoff.producer with requestId := 2, sessionId := "old-session", tool :=
        { Handoff.producer.tool with requestId := 2 } },
      question := question "ask:3" "retry-doc" 2 "old-session" "old?" }
  , { name := "wrong-reply-source", reply :=
        { replyEvidence with sourceDocId := "other" } }
  , { name := "wrong-reply-target", envelope :=
        { replyEnvelope with targetNodeDid := "other-node" }, reply :=
        { replyEvidence with nodeDid := "other-node" } }
  , { name := "wrong-reply-session", envelope :=
        { replyEnvelope with sessionId := some "other" } }
  , { name := "same-physical-request", reply :=
        { replyEvidence with requestDocId := Handoff.producer.requestDocId } }
  ]

private structure OwnerOutcome where
  handoffAccepted : Bool
  storedOpenReceipt : Bool
  linkedReplyAccepted : Bool
  deriving DecidableEq

private def ownerOutcome (test : Case) : OwnerOutcome :=
  let handoff := fileAndComplete? test.registry test.producer test.question 7
  let replyPost := handoff.bind fun receipt =>
    let item : Item := ⟨receipt.question.identity, .open, ""⟩
    claimLinkedReply? receipt item test.envelope test.reply
  let openReceipt := handoff.any fun receipt =>
    receipt.registry.rows.any fun row =>
      row.envelope == receipt.question && row.isOpen
  ⟨handoff.isSome, openReceipt, replyPost.isSome⟩

theorem generated_case_outcomes_follow_owners :
    cases.map (fun test => (test.name, ownerOutcome test)) =
      [ ("fresh-linked", ⟨true, true, true⟩)
      , ("request-mismatch", ⟨false, false, false⟩)
      , ("session-mismatch", ⟨false, false, false⟩)
      , ("tool-owner-mismatch", ⟨false, false, false⟩)
      , ("producer-agent-mismatch", ⟨false, false, false⟩)
      , ("producer-requester-mismatch", ⟨false, false, false⟩)
      , ("unstamped", ⟨false, false, false⟩)
      , ("terminal-old-row", ⟨false, false, false⟩)
      , ("wrong-reply-source", ⟨true, true, false⟩)
      , ("wrong-reply-target", ⟨true, true, false⟩)
      , ("wrong-reply-session", ⟨true, true, false⟩)
      , ("same-physical-request", ⟨true, true, false⟩)
      ] := by
  native_decide

structure HandoffInput where
  variant : String
  kind : String
  action : String
  content : String
  requestId : Nat
  producerRequestId : Nat
  toolRequestId : Nat
  producerNodeDid : String
  producerRequesterDid : String
  questionNodeDid : String
  questionRequesterDid : String
  sessionId : String
  requestDocId : String
  mailboxDocId : String
  replyRequestDocId : String
  replySourceDocId : String
  replySessionId : String
  replyBoundSessionId : Option String
  deriving Lean.ToJson

structure HandoffOutput where
  handoffAccepted : Bool
  storedOpenReceipt : Bool
  linkedReplyAccepted : Bool
  deriving Lean.ToJson

private def handoffInput (test : Case) : HandoffInput :=
  { variant := test.name, kind := test.question.identity.kind.toDefraDB
    action := test.question.handling.toDefraDB, content := test.question.content
    requestId := test.question.requestId, producerRequestId := test.producer.requestId
    toolRequestId := test.producer.tool.requestId, producerNodeDid := test.producer.nodeDid
    producerRequesterDid := test.producer.requesterDid
    questionNodeDid := test.question.identity.nodeDid
    questionRequesterDid := test.question.identity.requesterDid
    sessionId := test.question.sessionId, requestDocId := test.producer.requestDocId
    mailboxDocId := test.question.docId, replyRequestDocId := test.reply.requestDocId
    replySourceDocId := test.reply.sourceDocId, replySessionId := test.reply.sessionId
    replyBoundSessionId := test.envelope.sessionId }

private def handoffOutput (test : Case) : HandoffOutput :=
  let outcome := ownerOutcome test
  { handoffAccepted := outcome.handoffAccepted, storedOpenReceipt := outcome.storedOpenReceipt
    linkedReplyAccepted := outcome.linkedReplyAccepted }

private def handoffOf (i : HandoffInput) : HandoffOutput :=
  match cases.find? (·.name == i.variant) with
  | some test => handoffOutput test
  | none => { handoffAccepted := false, storedOpenReceipt := false, linkedReplyAccepted := false }

private def servedNames : List String := ["fresh-linked"]

/-- The served-node scenario files the question through the stamped mailbox
write from the producer's own claimed request and replies through the signed
reply claim, so it builds only the linked handoff. -/
@[conformance_family]
def linked : Family HandoffInput HandoffOutput where
  inputs := (cases.filter (servedNames.contains ·.name)).map handoffInput
  model := handoffOf

/-- model-production-gap: the stamped mailbox write takes the question's
request, session and owner from the filing request itself, so production
cannot file a question that names another request, session, tool owner or
producer; and no scenario replays the reply-side mismatches through the
signed reply claim after a linked handoff. -/
@[conformance_family]
def linkedUnreached : Family HandoffInput HandoffOutput where
  inputs := (cases.filter (!servedNames.contains ·.name)).map handoffInput
  model := handoffOf

end Conformance.MailboxHandoffContracts
