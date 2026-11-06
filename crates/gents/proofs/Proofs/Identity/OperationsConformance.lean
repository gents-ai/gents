import Proofs.Identity.Operations
import Proofs.Identity.RelationshipReplay
import Lean

namespace Identity.OperationsConformance
open Lean Identity.Household Identity.Operations

def bindingCases : List (String × CollectionBinding) :=
  [("absent", .absent), ("expected", .expected), ("unbound", .unbound),
   ("different", .different)]

structure RecoveryCase where
  name : String
  pending : Bool
  parentGranted : Bool
  runtime : String
  nativeOwner : Option String
  recordedParent : Option String
  currentParent : Option String
  grantSucceeded : Bool
  markSucceeded : Bool
  embeddedRuntime : String
  parentRegistered : Bool

def recoveryCases : List RecoveryCase :=
  [ ⟨"success", true, false, "runtime", some "runtime", some "node", some "node", true, true, "runtime", true⟩
  , ⟨"grant-failed", true, false, "runtime", some "runtime", some "node", some "node", false, true, "runtime", true⟩
  , ⟨"mark-failed", true, false, "runtime", some "runtime", some "node", some "node", true, false, "runtime", true⟩
  , ⟨"retry-mark", true, true, "runtime", some "runtime", some "node", some "node", false, true, "runtime", true⟩
  , ⟨"owner-unknown", true, false, "runtime", none, some "node", some "node", true, true, "runtime", true⟩
  , ⟨"owner-mismatch", true, false, "runtime", some "other", some "node", some "node", true, true, "runtime", true⟩
  , ⟨"runtime-missing", true, false, "", some "", some "node", some "node", true, true, "runtime", true⟩
  , ⟨"parent-missing", true, false, "runtime", some "runtime", none, none, true, true, "runtime", true⟩
  , ⟨"node-unknown", true, false, "runtime", some "runtime", some "node", none, true, true, "runtime", true⟩
  , ⟨"parent-mismatch", true, false, "runtime", some "runtime", some "node", some "other", true, true, "runtime", true⟩
  , ⟨"foreign-runtime", true, false, "runtime", some "runtime", some "node", some "node", true, true, "other", true⟩
  , ⟨"unregistered-parent", true, false, "runtime", some "runtime", some "node", some "node", true, true, "runtime", false⟩
  , ⟨"already-complete", false, false, "runtime", some "runtime", some "node", some "node", true, true, "runtime", true⟩ ]

def recoveryCaseJson (c : RecoveryCase) : Json :=
  let before : ParentPublication := ⟨true, c.parentGranted, !c.pending⟩
  let after := repairReadiness before c.runtime c.embeddedRuntime c.parentRegistered c.nativeOwner c.recordedParent c.currentParent
    c.grantSucceeded c.markSucceeded
  Json.mkObj [("name", toJson c.name), ("pending", toJson c.pending),
    ("parent_granted", toJson c.parentGranted), ("runtime", toJson c.runtime),
    ("embedded_runtime", toJson c.embeddedRuntime), ("parent_registered", toJson c.parentRegistered),
    ("native_owner", toJson c.nativeOwner), ("recorded_parent", toJson c.recordedParent),
    ("current_parent", toJson c.currentParent), ("grant_succeeded", toJson c.grantSucceeded),
    ("mark_succeeded", toJson c.markSucceeded), ("expected_parent_granted", toJson after.parentGranted),
    ("expected_parent_linked", toJson after.parentLinked)]

def preflightCasesJson : String := (toJson (bindingCases.map fun (name, binding) =>
  Json.mkObj [("name", toJson name), ("expected", toJson (readinessPreflight binding))])).compress

def recoveryCasesJson : String := (toJson (recoveryCases.map recoveryCaseJson)).compress

def secretPublicationCasesJson : String :=
  let before : State := { objects := [], relations := [], enrolled := [] }
  let after := publishSecret before "user" "runtime" "secret" "node" true false true true
  (toJson [Json.mkObj [("name", toJson "missing-node"),
    ("expected_created", toJson (after.objects.any (fun object =>
      object.resource == "secret_configuration" && object.id == "secret")))]]).compress

structure DeferredCase where
  name : String
  before : DeferredOperation
  runtime : DID
  embeddedRuntime : DID
  nativeOwner : Option DID
  node : Option String
  nodeRegistered : Bool
  committed : Bool

def deferredCases : List DeferredCase :=
  [ ⟨"before-node", beginDeferredOperation, "runtime", "runtime", some "runtime", none, false, true⟩
  , ⟨"success", beginDeferredOperation, "runtime", "runtime", some "runtime", some "node", true, true⟩
  , ⟨"unregistered-node", beginDeferredOperation, "runtime", "runtime", some "runtime", some "node", false, true⟩
  , ⟨"foreign-owner", beginDeferredOperation, "runtime", "runtime", some "other", some "node", true, true⟩
  , ⟨"foreign-runtime", beginDeferredOperation, "runtime", "other", some "runtime", some "node", true, true⟩
  , ⟨"failed-commit", beginDeferredOperation, "runtime", "runtime", some "runtime", some "node", true, false⟩
  , ⟨"revoked-after-publication", ⟨some "node", false, true⟩, "runtime", "runtime", some "runtime", some "node", true, true⟩ ]

def deferredCaseJson (c : DeferredCase) : Json :=
  let after := attachDeferredOperation c.before c.runtime c.embeddedRuntime c.nativeOwner c.node
    c.nodeRegistered c.committed
  Json.mkObj [("name", toJson c.name), ("runtime", toJson c.runtime),
    ("embedded_runtime", toJson c.embeddedRuntime), ("native_owner", toJson c.nativeOwner),
    ("node", toJson c.node), ("node_registered", toJson c.nodeRegistered),
    ("committed", toJson c.committed), ("before_parent", toJson c.before.parent),
    ("before_granted", toJson c.before.parentGranted), ("before_linked", toJson c.before.parentLinked),
    ("expected_parent", toJson after.parent), ("expected_granted", toJson after.parentGranted),
    ("expected_linked", toJson after.parentLinked)]

def deferredCasesJson : String := (toJson (deferredCases.map deferredCaseJson)).compress

def pairingPartitionCasesJson : String := (toJson
  ([("local", "runtime", "runtime", "peer"), ("foreign", "other", "other", "peer"),
    ("forged", "runtime", "other", "peer"), ("other-peer", "runtime", "runtime", "other-peer")].map fun (name, source, owner, peer) =>
    Json.mkObj [("name", toJson name), ("source", toJson source), ("peer", toJson peer),
      ("owner", toJson owner), ("selected", toJson (pairingInPartition "runtime" source owner peer "peer"))])).compress

def desiredPublisherCasesJson : String := (toJson
  ([("runtime", "runtime", "runtime", "runtime", true),
    ("client", "client", "client", "remote", true),
    ("foreign", "other", "client", "remote", true),
    ("unheld", "client", "client", "remote", false)].map
    fun (name, actor, localNode, targetNode, held) =>
      Json.mkObj [("name", toJson name), ("actor", toJson actor),
        ("local", toJson localNode), ("target", toJson targetNode), ("held", toJson held),
        ("allowed", toJson (desiredRoutePublisher actor localNode targetNode held))])).compress

def routeWitnessCasesJson : String := (toJson
  ([("valid", "node", "node", true, true), ("forged-desired", "other", "node", true, true),
    ("forged-applied", "node", "other", true, true), ("hidden", "node", "node", false, true)].map
    fun (name, desired, applied, desiredVisible, appliedVisible) =>
      Json.mkObj [("name", toJson name), ("desired_owner", toJson desired),
        ("applied_owner", toJson applied), ("desired_visible", toJson desiredVisible),
        ("applied_visible", toJson appliedVisible),
        ("allowed", toJson (runtimeRouteWitnesses "node" desired applied desiredVisible appliedVisible))])).compress

def receiverGrantCasesJson : String := (toJson
  ([(false, false), (true, false), (true, true)].map fun (senderGranted, delivered) =>
    Json.mkObj [("sender_granted", toJson senderGranted), ("delivered", toJson delivered),
      ("receiver_granted", toJson (receiverHasGrant senderGranted delivered))])).compress

def validObservation : ReadinessObservation :=
  ⟨"runtime", "runtime", 1, 1, some "node", some "user", some "runtime", some "node", true, true⟩

def observationCases : List (String × ReadinessObservation) :=
  [("valid", validObservation),
   ("foreign-runtime", { validObservation with embeddedRuntime := "other" }),
   ("no-node", { validObservation with nodeCount := 0 }),
   ("duplicate-node", { validObservation with nodeCount := 2 }),
   ("no-readiness", { validObservation with readinessCount := 0 }),
   ("duplicate-readiness", { validObservation with readinessCount := 2 }),
   ("node-unregistered", { validObservation with nodeOwner := none }),
   ("readiness-unregistered", { validObservation with readinessOwner := none }),
   ("foreign-readiness", { validObservation with readinessOwner := some "other" }),
   ("missing-parent", { validObservation with recordedParent := none }),
   ("replaced-parent", { validObservation with nodeId := some "replacement" }),
   ("pending", { validObservation with parentLinked := false }),
   ("revoked-after-publication", { validObservation with parentPresent := false })]

def observationCaseJson (name : String) (o : ReadinessObservation) : Json := Json.mkObj
  [("name", toJson name), ("requested_runtime", toJson o.requestedRuntime),
   ("embedded_runtime", toJson o.embeddedRuntime), ("node_count", toJson o.nodeCount),
   ("readiness_count", toJson o.readinessCount), ("node_id", toJson o.nodeId),
   ("node_owner", toJson o.nodeOwner), ("readiness_owner", toJson o.readinessOwner),
   ("recorded_parent", toJson o.recordedParent), ("parent_linked", toJson o.parentLinked),
   ("parent_present", toJson o.parentPresent), ("expected_usable", toJson (readinessObservationUsable o))]

def observationCasesJson : String := (toJson (observationCases.map fun (name, value) =>
  observationCaseJson name value)).compress

def settledCases : List (String × Option LiveReconcileStatus × Bool) :=
  [("missing", none, true), ("initial", some ⟨true, false, false⟩, true),
   ("idle-success", some ⟨true, true, false⟩, false),
   ("busy", some ⟨false, true, false⟩, true),
   ("idle-error", some ⟨true, true, true⟩, true)]

def settledCasesJson : String := (toJson (settledCases.map fun (name, status, persisted) =>
  Json.mkObj [("name", toJson name), ("live_present", toJson status.isSome),
    ("idle", toJson (status.any (fun (s : LiveReconcileStatus) => s.idle))),
    ("result_known", toJson (status.any (fun (s : LiveReconcileStatus) => s.lastResultKnown))),
    ("result_error", toJson (status.any (fun (s : LiveReconcileStatus) => s.lastResultError))),
    ("persisted_settled", toJson persisted),
    ("expected", toJson (goalRuntimeSettled status persisted))])).compress

structure PairingReaderCase where
  name : String
  before : PairingReaderPublication
  entitled : Bool
  ownerAuthorized : Bool
  committed : Bool
  generation : Nat

def pairingReaderCases : List PairingReaderCase :=
  [ ⟨"initial-grant", ⟨none, false⟩, true, true, true, 1⟩
  , ⟨"not-entitled", ⟨some 1, true⟩, false, true, true, 1⟩
  , ⟨"explicit-revocation", ⟨some 1, false⟩, true, true, true, 1⟩
  , ⟨"new-owner-authorization", ⟨some 1, false⟩, true, true, true, 2⟩
  , ⟨"unauthorized-generation", ⟨some 1, false⟩, true, false, true, 2⟩
  , ⟨"aborted-grant", ⟨none, false⟩, true, true, false, 1⟩
  , ⟨"aborted-revoke", ⟨some 1, true⟩, false, true, false, 1⟩ ]

def pairingReaderCaseJson (c : PairingReaderCase) : Json :=
  let after := reconcilePairingReader c.before c.entitled c.ownerAuthorized c.committed c.generation
  Json.mkObj [("name", toJson c.name), ("before_generation", toJson c.before.lastGrantedGeneration),
    ("before_granted", toJson c.before.readerGranted), ("entitled", toJson c.entitled),
    ("owner_authorized", toJson c.ownerAuthorized), ("committed", toJson c.committed),
    ("generation", toJson c.generation), ("expected_generation", toJson after.lastGrantedGeneration),
    ("expected_granted", toJson after.readerGranted)]

def pairingReaderCasesJson : String := (toJson (pairingReaderCases.map pairingReaderCaseJson)).compress

def pairingReaderPolicyCasesJson : String :=
  let route : Relation := ⟨"pairing_observations", "route", "route_reader", .actor "agent"⟩
  let tool : Relation := ⟨"pairing_observations", "route", "tool_reader", .actor "agent"⟩
  let manager : Relation := ⟨"pairing_observations", "route", "reader_manager", .actor "user"⟩
  let both := apply pairingReaderFixture (.grantAs "user" tool)
  let routeOnly := apply both (.revokeAs "user" tool)
  let neither := apply routeOnly (.revokeAs "user" route)
  let toolOnly := apply neither (.grantAs "user" tool)
  let noManager := apply neither (.revokeAs "runtime" manager)
  let cases : List (String × State × DID) :=
    [("runtime", pairingReaderFixture, "runtime"), ("user", pairingReaderFixture, "user"),
     ("agent", pairingReaderFixture, "agent"), ("sibling", pairingReaderFixture, "sibling"),
     ("tool-revoked", routeOnly, "agent"), ("both-revoked", neither, "agent"),
     ("tool-only", toolOnly, "agent"),
     ("manager-revoked", noManager, "user")]
  (toJson (cases.map fun (name, state, actor) => Json.mkObj
    [("name", toJson name), ("read", toJson (allowed 5 state actor "pairing_observations" "route" "read")),
     ("update", toJson (allowed 5 state actor "pairing_observations" "route" "update")),
     ("manage_reader", toJson (mayManage state actor route))])).compress

structure ReplayCase where
  name : String
  before : Identity.RelationshipReplay.Receiver
  event : Identity.RelationshipReplay.Event
  validSignature : Bool
  authenticatedTarget : Bool
  registered : Bool
  nativeManager : Bool
  committed : Bool

def replayReceiver : Identity.RelationshipReplay.Receiver :=
  ⟨"policy", "network", "document", "reader", "actor", "", "recipient", "", "recipient", 0, false⟩

def replayGrant : Identity.RelationshipReplay.Event :=
  ⟨"policy", "network", "document", "reader", "actor", "", "recipient", "", "recipient", "owner", 1, true⟩

def replayCases : List ReplayCase :=
  [ ⟨"grant", replayReceiver, replayGrant, true, true, true, true, true⟩
  , ⟨"invalid-signature", replayReceiver, replayGrant, false, true, true, true, true⟩
  , ⟨"wrong-transport-recipient", replayReceiver, replayGrant, true, false, true, true, true⟩
  , ⟨"unregistered-document", replayReceiver, replayGrant, true, true, false, true, true⟩
  , ⟨"unauthorized-manager", replayReceiver, replayGrant, true, true, true, false, true⟩
  , ⟨"aborted", replayReceiver, replayGrant, true, true, true, true, false⟩
  , ⟨"other-document", replayReceiver, { replayGrant with document := "other" }, true, true, true, true, true⟩
  , ⟨"other-subject", replayReceiver, { replayGrant with targetId := "other" }, true, true, true, true, true⟩
  , ⟨"other-recipient", replayReceiver, { replayGrant with recipient := "other" }, true, true, true, true, true⟩
  , ⟨"out-of-order", replayReceiver, { replayGrant with sequence := 2 }, true, true, true, true, true⟩
  , ⟨"revoke", { replayReceiver with sequence := 1, granted := true },
      { replayGrant with sequence := 2, add := false }, true, true, true, true, true⟩
  , ⟨"stale-grant", { replayReceiver with sequence := 2 }, replayGrant, true, true, true, true, true⟩ ]

def replayCaseJson (c : ReplayCase) : Json :=
  let after := Identity.RelationshipReplay.receive c.before c.event c.validSignature
    c.authenticatedTarget c.registered c.nativeManager c.committed
  let e := c.event
  let r := c.before
  Json.mkObj [("name", toJson c.name),
    ("before", Json.mkObj [("policy", toJson r.policy), ("resource", toJson r.resource),
      ("document", toJson r.document), ("relation", toJson r.relation),
      ("target_kind", toJson r.targetKind), ("target_resource", toJson r.targetResource),
      ("target_id", toJson r.targetId), ("target_relation", toJson r.targetRelation),
      ("recipient", toJson r.recipient), ("sequence", toJson r.sequence), ("granted", toJson r.granted)]),
    ("event", Json.mkObj [("policy", toJson e.policy), ("resource", toJson e.resource),
      ("document", toJson e.document), ("relation", toJson e.relation),
      ("target_kind", toJson e.targetKind), ("target_resource", toJson e.targetResource),
      ("target_id", toJson e.targetId), ("target_relation", toJson e.targetRelation),
      ("recipient", toJson e.recipient), ("signer", toJson e.signer),
      ("sequence", toJson e.sequence), ("add", toJson e.add)]),
    ("valid_signature", toJson c.validSignature), ("authenticated_target", toJson c.authenticatedTarget),
    ("registered", toJson c.registered), ("native_manager", toJson c.nativeManager),
    ("committed", toJson c.committed), ("expected_sequence", toJson after.sequence),
    ("expected_granted", toJson after.granted)]

def replayCasesJson : String := (toJson (replayCases.map replayCaseJson)).compress

def replayProducerCasesJson : String := (toJson
  ([(true, true, true), (false, true, true), (true, false, true), (true, true, false)].map
    fun (held, manager, committed) => Json.mkObj [("held_signer", toJson held),
      ("native_manager", toJson manager), ("committed", toJson committed),
      ("deliverable", toJson (Identity.RelationshipReplay.producerMayDeliver held manager committed))])).compress

def enrollmentPolicyCasesJson : String :=
  let revoked := apply enrollmentRequestWithParent (.revokeAs "candidate"
    ⟨"enrollment_records", "request", "reader", .actor "admin"⟩)
  let cases : List (String × State × DID × String × String) :=
    [("candidate-request-write", enrollmentRequestFixture, "candidate", "request", "update"),
     ("admin-request-read", enrollmentRequestFixture, "admin", "request", "read"),
     ("admin-request-write", enrollmentRequestFixture, "admin", "request", "update"),
     ("user-request-before-parent", enrollmentRequestFixture, "user", "request", "read"),
     ("user-request-after-parent", enrollmentRequestWithParent, "user", "request", "read"),
     ("outsider-request", enrollmentRequestWithParent, "outsider", "request", "read"),
     ("revoked-admin", revoked, "admin", "request", "read"),
     ("admin-terminal-write", enrollmentTerminalFixture, "admin", "terminal", "update"),
     ("candidate-terminal-read", enrollmentTerminalFixture, "candidate", "terminal", "read"),
     ("candidate-terminal-write", enrollmentTerminalFixture, "candidate", "terminal", "update"),
     ("user-terminal-read", enrollmentTerminalFixture, "user", "terminal", "read"),
     ("outsider-terminal", enrollmentTerminalFixture, "outsider", "terminal", "read")]
  (toJson (cases.map fun (name, state, actor, id, permission) =>
    Json.mkObj [("name", toJson name), ("actor", toJson actor), ("document", toJson id),
      ("permission", toJson permission),
      ("allowed", toJson (allowed 5 state actor "enrollment_records" id permission))])).compress

def networkProtocolCasesJson : String :=
  let collections := ["Network", "NetworkEnrollmentRequest", "NetworkEnrollmentDecision",
    "NetworkEnrollmentRouteReceipt", "NetworkAuthorizationRevision", "NodeDirectoryEntry",
    "PeerRegistry", "AgentSession"]
  (toJson (collections.map fun name => Json.mkObj [
    ("collection", toJson name), ("resource", toJson (networkProtocolResource name))])).compress

def replicaGrantCasesJson : String :=
  let before := privateReplicaFixture
  let pending := replicaGrant before "enrollment_records" "remote" "runtime" "local-node" "user"
    true true true false false
  let linked := replicaLinkPending pending "enrollment_records" "remote" "runtime" "local-node" "user"
    true true true
  let revoked := apply linked (.revoke ⟨"enrollment_records", "remote", "parent", .object "nodes" "local-node"⟩)
  let repaired := replicaLinkPending revoked "enrollment_records" "remote" "runtime" "local-node" "user"
    true true true
  let cases : List (String × State) := [("before", before), ("pending", pending),
    ("linked", linked), ("revoked", revoked), ("after-repair", repaired)]
  (toJson (cases.map fun (name, state) => Json.mkObj [("name", toJson name),
    ("runtime_read", toJson (allowed 5 state "runtime" "enrollment_records" "remote" "read")),
    ("user_read", toJson (allowed 5 state "user" "enrollment_records" "remote" "read")),
    ("user_write", toJson (allowed 5 state "user" "enrollment_records" "remote" "update")),
    ("agent_read", toJson (allowed 5 state "agent" "enrollment_records" "remote" "read")),
    ("outsider_read", toJson (allowed 5 state "outsider" "enrollment_records" "remote" "read")),
    ("app_read", toJson (allowed 5 state "runtime" "children" "application" "read")),
    ("pending", toJson (state.relations.contains ⟨"enrollment_records", "remote", "pending_parent", .actor "runtime"⟩))])).compress

def discoveryPolicyCasesJson : String :=
  let cases : List (String × State × String × String × String) :=
    [("outsider-read", publicDiscoveryFixture, "outsider", "directory", "read"),
     ("outsider-write", publicDiscoveryFixture, "outsider", "directory", "update"),
     ("local-user-write", localRegistryWriterFixture, "user", "self", "update"),
     ("revoked-user-write", localRegistryWriterRevoked, "user", "self", "update")]
  (toJson (cases.map fun (name, state, actor, id, permission) => Json.mkObj [("name", toJson name),
    ("allowed", toJson (allowed 5 state actor "discovery" id permission))])).compress

def localRegistryWriterCasesJson : String :=
  let cases := [("runtime-owner", "runtime", true), ("user-owner", "user", true),
    ("foreign-owner", "foreign", true), ("other-node", "user", false)]
  (toJson (cases.map fun (name, creator, namesRuntime) => Json.mkObj [("name", toJson name),
    ("creator", toJson creator), ("names_runtime", toJson namesRuntime),
    ("writer", toJson (localRegistryWriter creator "runtime" "user" namesRuntime))])).compress

end Identity.OperationsConformance
