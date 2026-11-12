import Proofs.Session.Request.RemoteDelivery
import Proofs.Conformance.Family

namespace Conformance.RemoteDelivery

open Lean Request Request.RemoteDelivery

deriving instance ToJson for CrossActorAuthorization.Observation

structure IntentWire where
  key : String
  digest : String
  sender : String
  target : String
  targetAgentDocId : String
  targetSessionDocId : Option String
  deriving ToJson

structure RecordWire where
  intent : IntentWire
  requestDocId : String
  owner : String
  workingAgent : String
  workingGrant : Bool
  deriving ToJson

def IntentWire.ofModel (intent : Intent) : IntentWire :=
  ⟨intent.key, intent.digest, intent.sender, intent.target, intent.targetAgentDoc,
    intent.targetSessionDoc⟩

def IntentWire.toModel (intent : IntentWire) : Intent :=
  ⟨intent.key, intent.digest, intent.sender, intent.target, intent.targetAgentDocId,
    intent.targetSessionDocId⟩

def RecordWire.ofModel (record : RemoteRecord) : RecordWire :=
  ⟨.ofModel record.intent, record.requestDocument, record.owner, record.workingAgent,
    record.workingAgentGranted⟩

def RecordWire.toModel (record : RecordWire) : RemoteRecord :=
  ⟨record.intent.toModel, record.requestDocId, record.owner, record.workingAgent, record.workingGrant⟩

structure DeliveryInput where
  name : String
  intent : IntentWire
  persisted : Bool
  existing : Option RecordWire
  createSucceeded : Bool
  workingGrant : Bool
  responseVerified : Bool
  deriving ToJson

structure DeliveryOutput where
  result : String
  confirmed : Bool
  deriving ToJson

example : (cases.find? (·.name == "committed-response-observed")).map (fun c =>
    resultName (deliver c.intent c.persisted c.existing c.createSucceeded c.workingGrant)) =
    some "committed" := by native_decide

/-- model-production-gap: production refuses every delivery to another
runtime until the authenticated HTTP delivery path exists
(`plan_with_delivery`), so no production path reaches remote creation,
grant or response confirmation. -/
@[conformance_family]
def deliveryUnreached : Family DeliveryInput DeliveryOutput where
  inputs := cases.map fun c =>
    { name := c.name, intent := .ofModel c.intent, persisted := c.persisted
      existing := c.existing.map .ofModel, createSucceeded := c.createSucceeded
      workingGrant := c.workingGrant, responseVerified := c.responseVerified }
  model := fun i =>
    let result := deliver i.intent.toModel i.persisted (i.existing.map RecordWire.toModel)
      i.createSucceeded i.workingGrant
    { result := resultName result, confirmed := confirmed result i.responseVerified }

structure ClaimInput where
  name : String
  record : RecordWire
  observation : CrossActorAuthorization.Observation
  commitSessionPermission : Bool
  commitAgentInvoke : Bool
  commitNodePermission : Bool
  nativeCommitted : Bool
  deriving ToJson

structure ClaimOutput where
  documentVisible : Bool
  atomicRequesterAuthorization : Bool
  decision : String
  claim : Bool
  deriving ToJson

def claimCases : List (String × RemoteRecord × CrossActorAuthorization.Observation × CrossActorAuthorization.NativeCommit) :=
  let intent := exampleIntent
  let valid : RemoteRecord := ⟨intent, "request-doc", "sender", "target", true⟩
  let observation : CrossActorAuthorization.Observation :=
    ⟨true, true, true, false, "request-doc", "session-doc", "agent-doc",
      some "sender", some "sender", "target", true, true, true, true, true⟩
  let commit : CrossActorAuthorization.NativeCommit := ⟨true, true, true, true⟩
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

example : (claimCases.find? (·.1 == "native-read-no-atomic-authority")).map (fun (_, record, observed, committed) =>
    (observed.visible, observed.supported, claim record observed committed)) =
    some (true, false, false) := by native_decide

/-- model-production-gap: production has no remote delivery claim; a
request from another runtime is never created, so no claim transaction
observes the sender's session, invoke or node permission. -/
@[conformance_family]
def claimUnreached : Family ClaimInput ClaimOutput where
  inputs := claimCases.map fun (name, record, observed, committed) =>
    { name, record := .ofModel record, observation := observed
      commitSessionPermission := committed.sessionPermission
      commitAgentInvoke := committed.agentInvoke
      commitNodePermission := committed.requesterNodePermission
      nativeCommitted := committed.committed }
  model := fun i =>
    let commit : CrossActorAuthorization.NativeCommit :=
      ⟨i.commitSessionPermission, i.commitAgentInvoke, i.nativeCommitted, i.commitNodePermission⟩
    { documentVisible := i.observation.visible
      atomicRequesterAuthorization := i.observation.supported
      decision := CrossActorAuthorization.decisionString (claimDecision i.record.toModel i.observation commit)
      claim := claim i.record.toModel i.observation commit }

end Conformance.RemoteDelivery
