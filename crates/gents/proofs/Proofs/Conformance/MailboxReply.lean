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
  , ("stale-question", { baselineEnvelope with questionCurrent := false }, baselineReply)
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
  ",\"question_current\":" ++ boolJson envelope.questionCurrent ++
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

private def savedAnswer : AnswerCacheBinding :=
  { client := "client", node := "node", agent := "agent", session := "session",
    item := "question", sourceItem := "question", confirmed := true }

private def closedQuestion : AnswerCacheObservation :=
  { requester := "client", node := "node", agent := "agent", session := "session",
    item := "question", kind := .ask, handling := .startRequest, status := .acted }

private def answerCacheCases : List
    (String × AnswerCacheBinding × Option AnswerCacheObservation × String × Bool) :=
  [ ("acted", savedAnswer, some closedQuestion, "client", true)
  , ("dismissed", savedAnswer, some { closedQuestion with status := .dismissed }, "client", true)
  , ("expired", savedAnswer, some { closedQuestion with status := .expired }, "client", true)
  , ("open", savedAnswer, some { closedQuestion with status := .open }, "client", true)
  , ("missing", savedAnswer, none, "client", true)
  , ("uncertain-delivery", { savedAnswer with confirmed := false }, some closedQuestion, "client", true)
  , ("acknowledgement-lost", savedAnswer, some closedQuestion, "client", false)
  , ("client-switched", savedAnswer, some closedQuestion, "other", true)
  , ("foreign-reader", savedAnswer, some { closedQuestion with requester := "other" }, "client", true)
  , ("wrong-node", savedAnswer, some { closedQuestion with node := "other" }, "client", true)
  , ("wrong-agent", savedAnswer, some { closedQuestion with agent := "other" }, "client", true)
  , ("wrong-session", savedAnswer, some { closedQuestion with session := "other" }, "client", true)
  , ("wrong-question", savedAnswer, some { closedQuestion with item := "other" }, "client", true)
  , ("wrong-kind", savedAnswer, some { closedQuestion with kind := .gate }, "client", true)
  , ("wrong-action", savedAnswer, some { closedQuestion with handling := .ack }, "client", true)
  , ("empty-binding", { savedAnswer with item := "", sourceItem := "" }, some closedQuestion, "client", true)
  , ("mismatched-authored-source", { savedAnswer with sourceItem := "other" }, some closedQuestion, "client", true)
  ]

def answerCacheCasesJson : String := jsonArray <| answerCacheCases.map fun
    (name, binding, observed, current, ack) =>
  let observation := observed.map fun value =>
    "{\"item\":{\"itemId\":" ++ jsonString value.item ++
    ",\"requesterDid\":" ++ jsonString value.requester ++
    ",\"targetNodeDid\":" ++ jsonString value.node ++
    ",\"targetAgentId\":" ++ jsonString value.agent ++
    ",\"sessionId\":" ++ jsonString value.session ++
    ",\"kind\":" ++ jsonString value.kind.toDefraDB ++
    ",\"action\":" ++ jsonString value.handling.toDefraDB ++
    ",\"status\":" ++ jsonString value.status.toDefraDB ++
    "},\"terminal\":" ++ boolJson value.status.terminal ++ "}"
  "{\"name\":" ++ jsonString name ++
  ",\"binding\":{\"clientDid\":" ++ jsonString binding.client ++
  ",\"nodeDid\":" ++ jsonString binding.node ++
  ",\"agentId\":" ++ jsonString binding.agent ++
  ",\"sessionId\":" ++ jsonString binding.session ++
  ",\"itemId\":" ++ jsonString binding.item ++
  ",\"sourceItemId\":" ++ jsonString binding.sourceItem ++
  ",\"confirmed\":" ++ boolJson binding.confirmed ++ "}" ++
  ",\"observation\":" ++ observation.getD "null" ++
  ",\"currentClient\":" ++ jsonString current ++
  ",\"acknowledged\":" ++ boolJson ack ++
  ",\"retire\":" ++ boolJson (mayRetireAnswerCache binding observed current ack) ++ "}"

end Conformance.MailboxReplyContracts
