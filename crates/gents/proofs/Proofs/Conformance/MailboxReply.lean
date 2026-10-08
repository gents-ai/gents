import Proofs.Mailbox.Reply
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.MailboxReplyContracts
open Mailbox Conformance.Contracts

private def baselineItem (status : Status) (receipt : String) : Item :=
  { identity :=
      { itemKey := "notice"
        requesterDid := "owner"
        nodeDid := "node"
        sourceKind := .session
        sourceId := "source"
        kind := .gate }
    status := status
    resolvedDocId := receipt }

private def baselineEnvelope : ReplyEnvelope :=
  { docId := "mailbox-doc", handling := .startRequest, targetNodeDid := "node", agentId := "repair",
    sessionId := some "session", deadlineValid := true }

private def baselineReply : ReplyEvidence :=
  { requestDocId := "request-doc", sourceDocId := "mailbox-doc", requesterDid := "owner",
    nodeDid := "node", agentId := "repair", sessionId := "session",
    authenticated := true, interactive := true }

private def variants : List (String × ReplyEnvelope × ReplyEvidence) :=
  [ ("matching", baselineEnvelope, baselineReply)
  , ("routed-agent", { baselineEnvelope with targetNodeDid := "repair-agent" },
      { baselineReply with nodeDid := "repair-agent" })
  , ("ack", { baselineEnvelope with handling := .ack }, baselineReply)
  , ("write-document", { baselineEnvelope with handling := .writeDocument }, baselineReply)
  , ("expired-deadline", { baselineEnvelope with deadlineValid := false }, baselineReply)
  , ("empty-mailbox", { baselineEnvelope with docId := "" }, baselineReply)
  , ("empty-target", { baselineEnvelope with agentId := "" }, baselineReply)
  , ("unbound-session", { baselineEnvelope with sessionId := none }, baselineReply)
  , ("unauthenticated", baselineEnvelope, { baselineReply with authenticated := false })
  , ("automated", baselineEnvelope, { baselineReply with interactive := false })
  , ("empty-request", baselineEnvelope, { baselineReply with requestDocId := "" })
  , ("empty-requester", baselineEnvelope, { baselineReply with requesterDid := "" })
  , ("foreign-requester", baselineEnvelope, { baselineReply with requesterDid := "other" })
  , ("foreign-agent", baselineEnvelope, { baselineReply with nodeDid := "other" })
  , ("empty-agent", baselineEnvelope, { baselineReply with nodeDid := "" })
  , ("wrong-agent", baselineEnvelope, { baselineReply with agentId := "other" })
  , ("wrong-session", baselineEnvelope, { baselineReply with sessionId := "other" })
  , ("wrong-source", baselineEnvelope, { baselineReply with sourceDocId := "other" })
  ]

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def caseJson (item : Item) (name : String) (envelope : ReplyEnvelope)
    (reply : ReplyEvidence) : String :=
  let post := claimReply? item envelope reply
  "{\"variant\":" ++ jsonString name ++
  ",\"status\":" ++ jsonString item.status.toDefraDB ++
  ",\"resolved_doc_id\":" ++ jsonString item.resolvedDocId ++
  ",\"handling\":" ++ jsonString envelope.handling.toDefraDB ++
  ",\"mailbox_doc_id\":" ++ jsonString envelope.docId ++
  ",\"target_agent_id\":" ++ jsonString envelope.agentId ++
  ",\"target_node_did\":" ++ jsonString envelope.targetNodeDid ++
  ",\"bound_session_id\":" ++ (envelope.sessionId.map jsonString).getD "null" ++
  ",\"deadline_valid\":" ++ boolJson envelope.deadlineValid ++
  ",\"request_doc_id\":" ++ jsonString reply.requestDocId ++
  ",\"source_doc_id\":" ++ jsonString reply.sourceDocId ++
  ",\"requester_did\":" ++ jsonString reply.requesterDid ++
  ",\"node_did\":" ++ jsonString reply.nodeDid ++
  ",\"agent_id\":" ++ jsonString reply.agentId ++
  ",\"session_id\":" ++ jsonString reply.sessionId ++
  ",\"authenticated\":" ++ boolJson reply.authenticated ++
  ",\"interactive\":" ++ boolJson reply.interactive ++
  ",\"accepted\":" ++ boolJson post.isSome ++
  ",\"post_status\":" ++ (post.map (jsonString ∘ Status.toDefraDB ∘ Item.status)).getD "null" ++
  ",\"post_resolved_doc_id\":" ++ (post.map (jsonString ∘ Item.resolvedDocId)).getD "null" ++ "}"

def casesJson : String := jsonArray <|
  [Status.open, .acted, .dismissed, .expired].flatMap fun status =>
    ["", "request-doc", "other-request"].flatMap fun receipt =>
      variants.map fun (name, envelope, reply) =>
        caseJson (baselineItem status receipt) name envelope reply

end Conformance.MailboxReplyContracts
