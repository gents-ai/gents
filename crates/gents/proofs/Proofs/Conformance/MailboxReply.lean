import Proofs.Mailbox.Reply
import Proofs.Mailbox.Executable
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
  { docId := "mailbox-doc", handling := .startRequest, targetNodeDid := "node", agentDid := "repair",
    sessionId := some "session", deadlineValid := true }

private def baselineReply : ReplyEvidence :=
  { requestDocId := "request-doc", sourceDocId := "mailbox-doc", requesterDid := "owner",
    nodeDid := "node", agentDid := "repair", sessionId := "session",
    authenticated := true, recipientAuthorized := true, interactive := true }

private def variants : List (String × ReplyEnvelope × ReplyEvidence) :=
  [ ("matching", baselineEnvelope, baselineReply)
  , ("routed-agent", { baselineEnvelope with targetNodeDid := "repair-agent" },
      { baselineReply with nodeDid := "repair-agent" })
  , ("ack", { baselineEnvelope with handling := .ack }, baselineReply)
  , ("write-document", { baselineEnvelope with handling := .writeDocument }, baselineReply)
  , ("expired-deadline", { baselineEnvelope with deadlineValid := false }, baselineReply)
  , ("empty-mailbox", { baselineEnvelope with docId := "" }, baselineReply)
  , ("empty-target", { baselineEnvelope with agentDid := "" }, baselineReply)
  , ("unbound-session", { baselineEnvelope with sessionId := none }, baselineReply)
  , ("new-client-session", { baselineEnvelope with sessionId := none }, { baselineReply with requesterDid := "other" })
  , ("unauthenticated", baselineEnvelope, { baselineReply with authenticated := false })
  , ("recipient-denied", baselineEnvelope, { baselineReply with recipientAuthorized := false })
  , ("automated", baselineEnvelope, { baselineReply with interactive := false })
  , ("empty-request", baselineEnvelope, { baselineReply with requestDocId := "" })
  , ("empty-requester", baselineEnvelope, { baselineReply with requesterDid := "" })
  , ("foreign-requester", baselineEnvelope, { baselineReply with requesterDid := "other" })
  , ("foreign-agent", baselineEnvelope, { baselineReply with nodeDid := "other" })
  , ("empty-agent", baselineEnvelope, { baselineReply with nodeDid := "" })
  , ("wrong-agent", baselineEnvelope, { baselineReply with agentDid := "other" })
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
  ",\"target_agent_did\":" ++ jsonString envelope.agentDid ++
  ",\"target_node_did\":" ++ jsonString envelope.targetNodeDid ++
  ",\"bound_session_id\":" ++ (envelope.sessionId.map jsonString).getD "null" ++
  ",\"deadline_valid\":" ++ boolJson envelope.deadlineValid ++
  ",\"question_current\":" ++ boolJson envelope.questionCurrent ++
  ",\"request_doc_id\":" ++ jsonString reply.requestDocId ++
  ",\"source_doc_id\":" ++ jsonString reply.sourceDocId ++
  ",\"requester_did\":" ++ jsonString reply.requesterDid ++
  ",\"node_did\":" ++ jsonString reply.nodeDid ++
  ",\"agent_did\":" ++ jsonString reply.agentDid ++
  ",\"session_id\":" ++ jsonString reply.sessionId ++
  ",\"authenticated\":" ++ boolJson reply.authenticated ++
  ",\"recipient_authorized\":" ++ boolJson reply.recipientAuthorized ++
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
    (String × AnswerCacheBinding × Option AnswerCacheObservation × String) :=
  [ ("acted", savedAnswer, some closedQuestion, "client")
  , ("dismissed", savedAnswer, some { closedQuestion with status := .dismissed }, "client")
  , ("expired", savedAnswer, some { closedQuestion with status := .expired }, "client")
  , ("open", savedAnswer, some { closedQuestion with status := .open }, "client")
  , ("missing", savedAnswer, none, "client")
  , ("unconfirmed", { savedAnswer with confirmed := false }, some closedQuestion, "client")
  , ("client-switched", savedAnswer, some closedQuestion, "other")
  , ("agent-created-session", savedAnswer, some { closedQuestion with requester := "other" }, "client")
  , ("wrong-node", savedAnswer, some { closedQuestion with node := "other" }, "client")
  , ("wrong-agent", savedAnswer, some { closedQuestion with agent := "other" }, "client")
  , ("wrong-session", savedAnswer, some { closedQuestion with session := "other" }, "client")
  , ("wrong-question", savedAnswer, some { closedQuestion with item := "other" }, "client")
  , ("wrong-kind", savedAnswer, some { closedQuestion with kind := .gate }, "client")
  , ("wrong-action", savedAnswer, some { closedQuestion with handling := .ack }, "client")
  , ("empty-binding", { savedAnswer with item := "", sourceItem := "" }, some closedQuestion, "client")
  , ("mismatched-authored-source", { savedAnswer with sourceItem := "other" }, some closedQuestion, "client")
  ]

/-- Every status the native owner can report reaches the retirement decision
with the otherwise-retiring binding, and exactly the terminal ones retire it, so
dropping a status row (such as `expired`) fails this build. -/
theorem answerCacheCases_cover_statuses :
    allStatuses.all (fun status => answerCacheCases.any fun (_, binding, observed, current) =>
      binding == savedAnswer && current == "client" &&
        observed.map (·.status) == some status &&
        mayRetireAnswerCache binding observed current == status.terminal) = true := by
  decide

private abbrev AnswerCacheCase :=
  String × AnswerCacheBinding × Option AnswerCacheObservation × String

/-- Some case breaks `broken` and keeps the answer. -/
private def keepsWhen (broken : AnswerCacheCase → Bool) : Bool :=
  answerCacheCases.any fun row =>
    let (_, b, o, c) := row
    broken row && !mayRetireAnswerCache b o c

/-- Every conjunct of `mayRetireAnswerCache` has a case that breaks it and keeps
the answer, so dropping a guard row fails this build. -/
theorem answerCacheCases_cover_guards :
    [ keepsWhen fun (_, b, _, _) => !b.confirmed
    , keepsWhen fun (_, b, _, c) => c != b.client
    , keepsWhen fun (_, b, _, _) => b.sourceItem != b.item
    , keepsWhen fun (_, b, _, _) => b.item == ""
    , keepsWhen fun (_, _, o, _) => o.isNone
    , keepsWhen fun (_, b, o, _) => o.any (·.node != b.node)
    , keepsWhen fun (_, b, o, _) => o.any (·.agent != b.agent)
    , keepsWhen fun (_, b, o, _) => o.any (·.session != b.session)
    , keepsWhen fun (_, b, o, _) => o.any (·.item != b.item)
    , keepsWhen fun (_, _, o, _) => o.any (·.kind != .ask)
    , keepsWhen fun (_, _, o, _) => o.any (·.handling != .startRequest)
    ].all id = true := by
  decide

private def answerCacheCaseJson
    (name : String) (binding : AnswerCacheBinding)
    (observed : Option AnswerCacheObservation) (current : String) : String :=
  let observation := observed.map fun value =>
    "{\"item\":{\"itemId\":" ++ jsonString value.item ++
    ",\"requesterDid\":" ++ jsonString value.requester ++
    ",\"targetNodeDid\":" ++ jsonString value.node ++
    ",\"targetAgentDid\":" ++ jsonString value.agent ++
    ",\"sessionId\":" ++ jsonString value.session ++
    ",\"kind\":" ++ jsonString value.kind.toDefraDB ++
    ",\"action\":" ++ jsonString value.handling.toDefraDB ++
    ",\"status\":" ++ jsonString value.status.toDefraDB ++
    "},\"terminal\":" ++ boolJson value.status.terminal ++ "}"
  "{\"name\":" ++ jsonString name ++
  ",\"binding\":{\"clientDid\":" ++ jsonString binding.client ++
  ",\"nodeDid\":" ++ jsonString binding.node ++
  ",\"agentDid\":" ++ jsonString binding.agent ++
  ",\"sessionId\":" ++ jsonString binding.session ++
  ",\"itemId\":" ++ jsonString binding.item ++
  ",\"sourceItemId\":" ++ jsonString binding.sourceItem ++
  ",\"confirmed\":" ++ boolJson binding.confirmed ++ "}" ++
  ",\"observation\":" ++ observation.getD "null" ++
  ",\"currentClient\":" ++ jsonString current ++
  ",\"retire\":" ++ boolJson (mayRetireAnswerCache binding observed current) ++ "}"

/-- The cases, plus the full status vocabulary with each status's terminality,
so a consumer checks status coverage against the enumeration rather than
against the case list itself. -/
def answerCacheCasesJson : String :=
  "{\"statuses\":" ++ jsonArray (allStatuses.map fun status =>
      "{\"status\":" ++ jsonString status.toDefraDB ++
      ",\"terminal\":" ++ boolJson status.terminal ++ "}") ++
  ",\"cases\":" ++ jsonArray (answerCacheCases.map fun
    (name, binding, observed, current) =>
      answerCacheCaseJson name binding observed current) ++ "}"

end Conformance.MailboxReplyContracts
