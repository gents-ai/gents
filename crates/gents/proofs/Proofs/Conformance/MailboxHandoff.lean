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

private def caseJson (test : Case) : String :=
  let handoff := fileAndComplete? test.registry test.producer test.question 7
  let replyPost := handoff.bind fun receipt =>
    let item : Item := ⟨receipt.question.identity, .open, ""⟩
    claimLinkedReply? receipt item test.envelope test.reply
  let openReceipt := handoff.any fun receipt =>
    receipt.registry.rows.any fun row =>
      row.envelope == receipt.question && row.isOpen
  "{\"variant\":" ++ jsonString test.name ++
    ",\"kind\":" ++ jsonString test.question.identity.kind.toDefraDB ++
    ",\"action\":" ++ jsonString test.question.handling.toDefraDB ++
    ",\"content\":" ++ jsonString test.question.content ++
    ",\"request_id\":" ++ toString test.question.requestId ++
    ",\"producer_request_id\":" ++ toString test.producer.requestId ++
    ",\"tool_request_id\":" ++ toString test.producer.tool.requestId ++
    ",\"session_id\":" ++ jsonString test.question.sessionId ++
    ",\"request_doc_id\":" ++ jsonString test.producer.requestDocId ++
    ",\"mailbox_doc_id\":" ++ jsonString test.question.docId ++
    ",\"reply_request_doc_id\":" ++ jsonString test.reply.requestDocId ++
    ",\"reply_source_doc_id\":" ++ jsonString test.reply.sourceDocId ++
    ",\"reply_session_id\":" ++ jsonString test.reply.sessionId ++
    ",\"reply_bound_session_id\":" ++
      (test.envelope.sessionId.map jsonString).getD "null" ++
    ",\"handoff_accepted\":" ++ boolJson handoff.isSome ++
    ",\"stored_open_receipt\":" ++ boolJson openReceipt ++
    ",\"linked_reply_accepted\":" ++ boolJson replyPost.isSome ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.MailboxHandoffContracts
