import Lean
import Proofs.Request.CrossActorAuthorization

namespace Request.RemoteDelivery

/-- The accepted tool call retains the exact signed request before any send.
The digest represents its complete bytes, including its stable retry key and
the physical target Agent and session identifiers. Signature verification must
cover both identifiers; their presence in metadata alone is insufficient. -/
structure Intent where
  key : String
  digest : String
  sender : String
  target : String
  targetAgentDoc : String
  targetSessionDoc : Option String
  deriving BEq, Repr

structure RemoteRecord where
  intent : Intent
  requestDocument : String
  owner : String
  workingAgent : String
  workingAgentGranted : Bool
  deriving BEq, Repr

inductive Result where
  | wait | retry | pendingGrant | conflict | committed | existing
  deriving BEq, Repr

/-- Native ACP does not by itself bind document creation to a target. Creation
and the subsequent working-agent grant are separate effects; a crash may leave
a retained request whose grant must be retried. A native grant denial does not
show that an earlier create failed. Claim authorization remains a separate
transaction-bound check of the immediate signed author. -/
def deliver (intent : Intent) (persisted : Bool) (existing : Option RemoteRecord)
    (createSucceeded workingGrant : Bool) : Result :=
  if !persisted then .wait
  else match existing with
    | some record =>
        if record.intent == intent && record.owner == intent.sender &&
            record.workingAgent == intent.target then
          if record.workingAgentGranted then .existing else .pendingGrant
        else .conflict
    | none =>
        if !createSucceeded then .retry
        else if !workingGrant then .pendingGrant
        else .committed

/-- A transport response confirms a delivery only after its exact binding and
selected target-agent signature have been verified. Loss of that response is
an uncertain attempt, not proof that creation failed. -/
def confirmed (result : Result) (verifiedTargetResponse : Bool) : Bool :=
  verifiedTargetResponse && (result == .committed || result == .existing)

/-- Native authorization observes the immediate sender's session, Agent invoke,
and node permissions inside the target Agent's claim transaction. The same
native commit fences those observations; visibility or a prior detached check
is insufficient. The CrossActor owner determines denial, retry, and claim. -/
def claimDecision (record : RemoteRecord)
    (observation : CrossActorAuthorization.Observation)
    (commit : CrossActorAuthorization.NativeCommit) : CrossActorAuthorization.Decision :=
  match record.intent.targetSessionDoc with
  | none => .unavailable
  | some session =>
      if record.owner != record.intent.sender || record.workingAgent != record.intent.target then
        .denied
      else
        CrossActorAuthorization.decideClaim
          ⟨record.requestDocument, session, record.intent.targetAgentDoc,
            record.intent.sender, record.intent.target⟩ observation commit

def claim (record : RemoteRecord) (observation : CrossActorAuthorization.Observation)
    (commit : CrossActorAuthorization.NativeCommit) : Bool :=
  claimDecision record observation commit == .claimed

theorem missing_native_authorization_blocks_claim (record : RemoteRecord)
    (observation : CrossActorAuthorization.Observation)
    (commit : CrossActorAuthorization.NativeCommit) (unsupported : observation.supported = false) :
    claim record observation commit = false := by
  cases h : record.intent.targetSessionDoc with
  | none => simp [claim, claimDecision, h] <;> rfl
  | some session =>
      simp [claim, claimDecision, h, CrossActorAuthorization.decideClaim, unsupported]
      all_goals split <;> rfl

/-- A parent tuple on a sender-owned document can be managed by that sender.
Its presence alone is not proof of authorization to invoke the target Agent. -/
def parentTupleProvesInvoke (_parentPresent : Bool) : Bool := false

theorem forged_parent_is_not_invoke_evidence : parentTupleProvesInvoke true = false := by rfl

/-- A detached native observation has no modeled commit-conflict fence. It may
still be true after permission has been revoked before the work executes. -/
def detachedObservationMayBeStale (observed current : Bool) : Bool := observed && !current

theorem revoke_after_observation_can_be_missed :
    detachedObservationMayBeStale true false = true := by rfl

/-- A tool call owns its persisted intent and records uncertainty before HTTP.
Recovery must reuse the exact request and its physical target bindings. -/
structure ToolCall where
  intent : Option Intent
  attempted : Bool
  receipt : Option RemoteRecord
  deriving BEq, Repr

def signedTargetsPresent (intent : Intent) : Bool :=
  !intent.targetAgentDoc.isEmpty &&
    (intent.targetSessionDoc.any fun session => !session.isEmpty)

/-- Signature validity is the external cryptographic verifier's result over
all signed request fields, including the physical target identifiers. -/
def reserve (call : ToolCall) (intent : Intent) (signatureValid : Bool) : Option ToolCall :=
  if !signatureValid || !signedTargetsPresent intent then none
  else match call.intent with
    | none => if call.attempted || call.receipt.isSome then none
        else some { call with intent := some intent }
    | some previous => if previous == intent then some call else none

def reserveSigned (call : ToolCall) (intent : Intent) (signatureValid : Bool) : Option ToolCall :=
  reserve call intent signatureValid

theorem unsigned_intent_cannot_be_reserved (call : ToolCall) (intent : Intent) :
    reserve call intent false = none := by simp [reserve]

def sent (call : ToolCall) : ToolCall :=
  if call.intent.isSome then { call with attempted := true } else call

def observe (call : ToolCall) (record : RemoteRecord) : ToolCall :=
  if !call.receipt.isSome && call.attempted && call.intent == some record.intent && record.owner == record.intent.sender &&
      record.workingAgent == record.intent.target && record.workingAgentGranted then
    { call with receipt := some record }
  else call

/-- The signed terminal payload includes the exact request and physical targets,
the target request document, its terminal status, and its output. The verifier
checks the signature over this complete payload under the selected Agent DID. -/
structure TerminalPayload where
  intent : Intent
  requestDocument : String
  status : String
  output : String
  deriving BEq, Repr

structure Response where
  payload : TerminalPayload
  signedPayload : TerminalPayload
  signer : String
  signatureValid : Bool
  deriving BEq, Repr

def acceptResponse (call : ToolCall) (response : Response) : Bool :=
  match call.receipt with
  | none => false
  | some receipt =>
      call.intent == some response.payload.intent &&
        receipt.intent == response.payload.intent &&
        receipt.requestDocument == response.payload.requestDocument &&
        response.payload == response.signedPayload &&
        response.signer == response.payload.intent.target && response.signatureValid

theorem no_send_without_intent (call : ToolCall) (missing : call.intent = none) :
    sent call = call := by simp [sent, missing]

theorem retry_preserves_intent (call : ToolCall) : (sent call).intent = call.intent := by
  simp only [sent]; split <;> rfl

theorem no_receipt_without_attempt (call : ToolCall) (record : RemoteRecord)
    (unattempted : call.attempted = false) : observe call record = call := by
  simp [observe, unattempted]

theorem unsigned_response_is_rejected (call : ToolCall) (payload signed : TerminalPayload)
    (signer : String) : acceptResponse call ⟨payload, signed, signer, false⟩ = false := by
  cases h : call.receipt <;> simp [acceptResponse, h]

theorem no_unpersisted_delivery (intent : Intent) (existing : Option RemoteRecord)
    (create grant : Bool) : deliver intent false existing create grant = .wait := by rfl

theorem missing_post_create_grant_stays_pending (intent : Intent) :
    deliver intent true none true false = .pendingGrant := by rfl

theorem unknown_response_cannot_confirm (result : Result) :
    confirmed result false = false := by rfl

theorem mismatched_intent_cannot_confirm_existing (wanted actual : Intent)
    (different : (actual == wanted) = false) :
    deliver wanted true (some ⟨actual, "request-doc", actual.sender, actual.target, true⟩) true true = .conflict := by
  simp [deliver, different]

open Lean

def resultName : Result → String
  | .wait => "wait"
  | .retry => "retry"
  | .pendingGrant => "pending_grant"
  | .conflict => "conflict"
  | .committed => "committed"
  | .existing => "existing"

structure Case where
  name : String
  intent : Intent
  persisted : Bool
  existing : Option RemoteRecord
  createSucceeded : Bool
  workingGrant : Bool
  responseVerified : Bool

def exampleIntent : Intent := ⟨"tool-1", "signed-request-1", "sender", "target", "agent-doc", some "session-doc"⟩

def cases : List Case :=
  let request := exampleIntent
  let otherTarget : Intent := ⟨"tool-1", "signed-request-1", "sender", "other-target", "other-agent-doc", some "session-doc"⟩
  [ ⟨"intent-not-durable", request, false, none, true, true, true⟩
  , ⟨"create-unconfirmed", request, true, none, false, false, false⟩
  , ⟨"grant-not-yet-applied", request, true, none, true, false, false⟩
  , ⟨"committed-response-lost", request, true, none, true, true, false⟩
  , ⟨"committed-response-observed", request, true, none, true, true, true⟩
  , ⟨"verified-replay", request, true, some ⟨request, "request-doc", "sender", "target", true⟩, false, false, true⟩
  , ⟨"same-key-different-target", request, true, some ⟨otherTarget, "request-doc", "sender", "other-target", true⟩, true, true, true⟩
  , ⟨"existing-with-wrong-owner", request, true, some ⟨request, "request-doc", "other", "target", true⟩, true, true, true⟩
  , ⟨"existing-with-wrong-worker", request, true, some ⟨request, "request-doc", "sender", "other-target", true⟩, true, true, true⟩
  , ⟨"existing-without-working-grant", request, true, some ⟨request, "request-doc", "sender", "target", false⟩, true, false, false⟩ ]

def intentJson (intent : Intent) : Json := Json.mkObj
  [("key", toJson intent.key), ("digest", toJson intent.digest),
   ("sender", toJson intent.sender), ("target", toJson intent.target),
   ("target_agent_doc_id", toJson intent.targetAgentDoc),
   ("target_session_doc_id", toJson intent.targetSessionDoc)]

def recordJson (record : RemoteRecord) : Json := Json.mkObj
  [("intent", intentJson record.intent), ("request_doc_id", toJson record.requestDocument),
   ("owner", toJson record.owner),
   ("working_agent", toJson record.workingAgent), ("working_grant", toJson record.workingAgentGranted)]

def claimCasesJson : String :=
  let intent := exampleIntent
  let valid : RemoteRecord := ⟨intent, "request-doc", "sender", "target", true⟩
  let observation : CrossActorAuthorization.Observation :=
    ⟨true, true, true, false, "request-doc", "session-doc", "agent-doc",
      some "sender", some "sender", "target", true, true, true, true, true⟩
  let commit : CrossActorAuthorization.NativeCommit := ⟨true, true, true, true⟩
  let cases : List (String × RemoteRecord × CrossActorAuthorization.Observation × CrossActorAuthorization.NativeCommit) :=
    [("native-read-no-atomic-authority", valid, { observation with supported := false }, commit),
     ("transaction-authorized", valid, observation, commit),
     ("hidden-document", valid, { observation with visible := false }, commit),
     ("invalid-signature", valid, { observation with signatureValid := false }, commit),
     ("wrong-owner", valid, { observation with owner := some "other" }, commit),
     ("wrong-author", valid, { observation with authenticatedAuthor := some "other" }, commit),
     ("wrong-agent", valid, { observation with executingAgent := "other" }, commit),
     ("wrong-session", valid, { observation with session := "other" }, commit),
     ("wrong-agent-document", valid, { observation with agentDocument := "other" }, commit),
     ("session-revoked-before-commit", valid, observation, { commit with sessionPermission := false }),
     ("invoke-revoked-before-commit", valid, observation, { commit with agentInvoke := false }),
     ("node-revoked-before-commit", valid, observation, { commit with requesterNodePermission := false }),
     ("grant-not-claim-proof", { valid with workingAgentGranted := false },
       { observation with supported := false }, commit)]
  (toJson (cases.map fun (name, record, observed, committed) =>
    Json.mkObj [("name", toJson name), ("record", recordJson record),
      ("observation", CrossActorAuthorization.observationJson observed),
      ("commit_session_permission", toJson committed.sessionPermission),
      ("commit_agent_invoke", toJson committed.agentInvoke),
      ("commit_node_permission", toJson committed.requesterNodePermission),
      ("native_committed", toJson committed.committed),
      ("document_visible", toJson observed.visible),
      ("atomic_requester_authorization", toJson observed.supported),
      ("decision", toJson (CrossActorAuthorization.decisionString (claimDecision record observed committed))),
      ("claim", toJson (claim record observed committed))])).compress

def caseJson (case_ : Case) : Json :=
  let result := deliver case_.intent case_.persisted case_.existing case_.createSucceeded case_.workingGrant
  Json.mkObj [("name", toJson case_.name), ("intent", intentJson case_.intent),
    ("persisted", toJson case_.persisted),
    ("existing", case_.existing.map recordJson |>.getD Json.null),
    ("create_succeeded", toJson case_.createSucceeded), ("working_grant", toJson case_.workingGrant),
    ("response_verified", toJson case_.responseVerified),
    ("result", toJson (resultName result)),
    ("confirmed", toJson (confirmed result case_.responseVerified))]

def casesJson : String := (toJson (cases.map caseJson)).compress

def toolCallJson (call : ToolCall) : Json := Json.mkObj
  [("intent", call.intent.map intentJson |>.getD Json.null),
   ("attempted", toJson call.attempted),
   ("receipt", call.receipt.map recordJson |>.getD Json.null)]

def recoveryCasesJson : String :=
  let intent := exampleIntent
  let changed : Intent := { intent with digest := "different-request" }
  let changedTarget : Intent := { intent with targetAgentDoc := "different-agent-doc" }
  let empty : ToolCall := ⟨none, false, none⟩
  let reserved := (reserve empty intent true).getD empty
  let attempted := sent reserved
  let wrong : RemoteRecord := ⟨changed, "request-doc", "sender", "target", true⟩
  let valid : RemoteRecord := ⟨intent, "request-doc", "sender", "target", true⟩
  let retained := observe attempted valid
  let changedDocument : RemoteRecord := { valid with requestDocument := "other-request-doc" }
  (toJson [Json.mkObj [("name", toJson "reserve-before-send"),
      ("before", toolCallJson empty), ("after", toolCallJson reserved)],
    Json.mkObj [("name", toJson "lost-response"), ("before", toolCallJson reserved),
      ("after", toolCallJson attempted)],
    Json.mkObj [("name", toJson "changed-retry-refused"),
      ("before", toolCallJson attempted),
      ("after", (reserve attempted changed true).map toolCallJson |>.getD Json.null)],
    Json.mkObj [("name", toJson "changed-target-refused"),
      ("before", toolCallJson attempted),
      ("after", (reserveSigned attempted changedTarget true).map toolCallJson |>.getD Json.null)],
    Json.mkObj [("name", toJson "forged-signature-refused"),
      ("before", toolCallJson attempted),
      ("after", (reserveSigned attempted intent false).map toolCallJson |>.getD Json.null)],
    Json.mkObj [("name", toJson "same-intent-retained"),
      ("before", toolCallJson attempted),
      ("after", (reserveSigned attempted intent true).map toolCallJson |>.getD Json.null)],
    Json.mkObj [("name", toJson "wrong-receipt-ignored"),
      ("before", toolCallJson attempted), ("after", toolCallJson (observe attempted wrong))],
    Json.mkObj [("name", toJson "verified-receipt-retained"),
      ("before", toolCallJson attempted), ("after", toolCallJson retained)],
    Json.mkObj [("name", toJson "changed-receipt-document-ignored"),
      ("before", toolCallJson retained), ("after", toolCallJson (observe retained changedDocument))]]).compress

def terminalPayloadJson (payload : TerminalPayload) : Json := Json.mkObj
  [("intent", intentJson payload.intent), ("request_doc_id", toJson payload.requestDocument),
   ("status", toJson payload.status), ("output", toJson payload.output)]

def responseCasesJson : String :=
  let intent := exampleIntent
  let payload : TerminalPayload := ⟨intent, "request-doc", "completed", "answer"⟩
  let call : ToolCall := ⟨some intent, true, some ⟨intent, "request-doc", "sender", "target", true⟩⟩
  let responses : List (String × Response) :=
    [("target-signed", ⟨payload, payload, "target", true⟩),
     ("runtime-signed", ⟨payload, payload, "runtime", true⟩),
     ("unsigned", ⟨payload, payload, "target", false⟩),
     ("different-request", ⟨{ payload with requestDocument := "other" },
       { payload with requestDocument := "other" }, "target", true⟩),
     ("different-target", ⟨{ payload with intent := { intent with targetAgentDoc := "other" } },
       { payload with intent := { intent with targetAgentDoc := "other" } }, "target", true⟩),
     ("altered-output", ⟨{ payload with output := "altered" }, payload, "target", true⟩),
     ("altered-status", ⟨{ payload with status := "failed" }, payload, "target", true⟩)]
  (toJson (responses.map fun (name, response) => Json.mkObj
    [("name", toJson name), ("payload", terminalPayloadJson response.payload),
     ("signed_payload", terminalPayloadJson response.signedPayload),
     ("signer", toJson response.signer), ("signature_valid", toJson response.signatureValid),
     ("accepted", toJson (acceptResponse call response))])).compress

end Request.RemoteDelivery
