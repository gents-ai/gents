import Proofs.Mailbox.Handoff
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.MailboxHandoffContracts
open Mailbox Mailbox.Handoff Conformance.Contracts

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

/-- Inputs vary one handoff boundary at a time. Expectations below are
evaluated through the mailbox, tool, lease, and reply owners. -/
private def cases : List Case :=
  [ { name := "fresh-linked" }
  , { name := "request-mismatch", question := { openQuestion with requestId := 2 } }
  , { name := "session-mismatch", question := { openQuestion with sessionId := "other" } }
  , { name := "tool-owner-mismatch", producer :=
        { Handoff.producer with tool := { Handoff.producer.tool with requestId := 2 } } }
  , { name := "producer-agent-mismatch", producer :=
        { Handoff.producer with agentDid := "other-agent" } }
  , { name := "producer-requester-mismatch", producer :=
        { Handoff.producer with requesterDid := "other-requester" } }
  , { name := "unstamped", question :=
        { openQuestion with context := ⟨"foreign", "agent"⟩ } }
  , { name := "terminal-old-row", registry := staleRegistry,
      producer := { Handoff.producer with requestId := 2, sessionId := "old-session", tool :=
        { Handoff.producer.tool with requestId := 2 } },
      question := question "ask:3" "retry-doc" 2 "old-session" "old?" }
  , { name := "wrong-reply-source", reply :=
        { replyEvidence with sourceDocId := "other" } }
  , { name := "wrong-reply-target", envelope :=
        { replyEnvelope with targetAgentDid := "other-agent" }, reply :=
        { replyEvidence with agentDid := "other-agent" } }
  , { name := "wrong-reply-session", envelope :=
        { replyEnvelope with sessionId := some "other" } }
  , { name := "same-physical-request", reply :=
        { replyEvidence with requestDocId := Handoff.producer.requestDocId } }
  ]

private def boolJson (value : Bool) : String := if value then "true" else "false"

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

/-- Regression expectations are checked against the composed executable
owners, not copied into the JSON exporter or a Rust-side policy machine. -/
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

private def caseJson (test : Case) : String :=
  let outcome := ownerOutcome test
  "{\"variant\":" ++ jsonString test.name ++
    ",\"kind\":" ++ jsonString test.question.identity.kind.toDefraDB ++
    ",\"action\":" ++ jsonString test.question.handling.toDefraDB ++
    ",\"content\":" ++ jsonString test.question.content ++
    ",\"request_id\":" ++ toString test.question.requestId ++
    ",\"producer_request_id\":" ++ toString test.producer.requestId ++
    ",\"tool_request_id\":" ++ toString test.producer.tool.requestId ++
    ",\"producer_agent_did\":" ++ jsonString test.producer.agentDid ++
    ",\"producer_requester_did\":" ++ jsonString test.producer.requesterDid ++
    ",\"question_agent_did\":" ++ jsonString test.question.identity.agentDid ++
    ",\"question_requester_did\":" ++ jsonString test.question.identity.requesterDid ++
    ",\"session_id\":" ++ jsonString test.question.sessionId ++
    ",\"request_doc_id\":" ++ jsonString test.producer.requestDocId ++
    ",\"mailbox_doc_id\":" ++ jsonString test.question.docId ++
    ",\"reply_request_doc_id\":" ++ jsonString test.reply.requestDocId ++
    ",\"reply_source_doc_id\":" ++ jsonString test.reply.sourceDocId ++
    ",\"reply_session_id\":" ++ jsonString test.reply.sessionId ++
    ",\"reply_bound_session_id\":" ++
      (test.envelope.sessionId.map jsonString).getD "null" ++
    ",\"handoff_accepted\":" ++ boolJson outcome.handoffAccepted ++
    ",\"stored_open_receipt\":" ++ boolJson outcome.storedOpenReceipt ++
    ",\"linked_reply_accepted\":" ++ boolJson outcome.linkedReplyAccepted ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.MailboxHandoffContracts
