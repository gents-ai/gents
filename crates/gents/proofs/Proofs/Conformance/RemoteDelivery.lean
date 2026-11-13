import Proofs.Session.Request.RemoteDelivery
import Proofs.Conformance.Family

namespace Conformance.RemoteDelivery

open Lean Request.RemoteDelivery

deriving instance ToJson for Intent, RemoteRecord, ToolCall, TerminalPayload, Response

inductive Operation where
  | reserve
  | send
  | observe
  deriving ToJson

structure RecoveryInput where
  name : String
  before : ToolCall
  operation : Operation
  intent : Option Intent
  signatureValid : Bool
  record : Option RemoteRecord
  deriving ToJson

private def reserveInput (name : String) (before : ToolCall) (intent : Intent)
    (signatureValid : Bool) : RecoveryInput :=
  { name, before, operation := .reserve, intent := some intent, signatureValid, record := none }

private def observeInput (name : String) (before : ToolCall) (record : RemoteRecord) :
    RecoveryInput :=
  { name, before, operation := .observe, intent := none, signatureValid := false,
    record := some record }

private def recoveryInputs : List RecoveryInput :=
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
  [ reserveInput "reserve-before-send" empty intent true
  , { name := "lost-response", before := reserved, operation := .send, intent := none,
      signatureValid := false, record := none }
  , reserveInput "changed-retry-refused" attempted changed true
  , reserveInput "changed-target-refused" attempted changedTarget true
  , reserveInput "forged-signature-refused" attempted intent false
  , reserveInput "same-intent-retained" attempted intent true
  , observeInput "wrong-receipt-ignored" attempted wrong
  , observeInput "verified-receipt-retained" attempted valid
  , observeInput "changed-receipt-document-ignored" retained changedDocument ]

/-- model-production-gap: the session-message owner refuses remote Agent
delivery until the authenticated HTTP delivery path exists
(`plan_with_delivery` returns its "authenticated HTTP delivery path" refusal),
so no production tool call reserves a signed remote intent, records a send
attempt or retains a target receipt. -/
@[conformance_family]
def recoveryUnreached : Family RecoveryInput (Option ToolCall) where
  inputs := recoveryInputs
  model := fun i =>
    match i.operation with
    | .reserve => i.intent.bind fun intent => reserveSigned i.before intent i.signatureValid
    | .send => some (sent i.before)
    | .observe => i.record.map (observe i.before)

structure ResponseInput where
  name : String
  call : ToolCall
  response : Response
  deriving ToJson

private def responseInputs : List ResponseInput :=
  let intent := exampleIntent
  let payload : TerminalPayload := ⟨intent, "request-doc", "completed", "answer"⟩
  let call : ToolCall := ⟨some intent, true, some ⟨intent, "request-doc", "sender", "target", true⟩⟩
  let other := { payload with requestDocument := "other" }
  let otherTarget := { payload with intent := { intent with targetAgentDoc := "other" } }
  [ ("target-signed", ⟨payload, payload, "target", true⟩)
  , ("runtime-signed", ⟨payload, payload, "runtime", true⟩)
  , ("unsigned", ⟨payload, payload, "target", false⟩)
  , ("different-request", ⟨other, other, "target", true⟩)
  , ("different-target", ⟨otherTarget, otherTarget, "target", true⟩)
  , ("altered-output", ⟨{ payload with output := "altered" }, payload, "target", true⟩)
  , ("altered-status", ⟨{ payload with status := "failed" }, payload, "target", true⟩) ].map
    fun (name, response) => { name, call, response }

/-- model-production-gap: remote Agent delivery is refused before any send
(`plan_with_delivery`), so production never receives a target-signed terminal
response to verify against its retained receipt. -/
@[conformance_family]
def responseUnreached : Family ResponseInput Bool where
  inputs := responseInputs
  model := fun i => acceptResponse i.call i.response

end Conformance.RemoteDelivery
