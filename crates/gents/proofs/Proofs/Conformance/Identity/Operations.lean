import Proofs.Store.Identity.OperationsConformance
import Proofs.Conformance.Family

namespace Conformance.IdentityOperations

open Lean Identity.Household Identity.Operations Identity.OperationsConformance

structure PublisherInput where
  name : String
  actor : String
  «local» : String
  target : String
  held : Bool
  deriving ToJson

structure Allowed where
  allowed : Bool
  deriving ToJson

def publisherInputs : List PublisherInput :=
  [ { name := "runtime", actor := "runtime", «local» := "runtime", target := "runtime", held := true }
  , { name := "client", actor := "client", «local» := "client", target := "remote", held := true }
  , { name := "foreign", actor := "other", «local» := "client", target := "remote", held := true }
  , { name := "unheld", actor := "client", «local» := "client", target := "remote", held := false } ]

def publisherAllowed (i : PublisherInput) : Allowed :=
  { allowed := desiredRoutePublisher i.actor i.«local» i.target i.held }

/-- The materialization scenario writes as the held local Node, or as a
remote identity whose signer the store does not hold, toward the local or a
remote Node. -/
@[conformance_family]
def desiredPublisher : Family PublisherInput Allowed where
  inputs := publisherInputs.filter (·.name ≠ "foreign")
  model := publisherAllowed

/-- model-production-gap: a store holds only its own Node's signer, so no
production path writes as a foreign actor whose signer is held. -/
@[conformance_family]
def desiredPublisherUnreached : Family PublisherInput Allowed where
  inputs := publisherInputs.filter (·.name = "foreign")
  model := publisherAllowed

structure DiscoveryInput where
  name : String
  actor : String
  document : String
  permission : String
  deriving ToJson

def discoveryState : String → State
  | "local-user-write" => localRegistryWriterFixture
  | "revoked-user-write" => localRegistryWriterRevoked
  | _ => publicDiscoveryFixture

/-- The replicated registry scenario reads and writes the public directory as
an outsider and the local registry row as the Node's user, before and after
the user's writer relation is revoked. -/
@[conformance_family]
def discovery : Family DiscoveryInput Allowed where
  inputs :=
    [ { name := "outsider-read", actor := "outsider", document := "directory", permission := "read" }
    , { name := "outsider-write", actor := "outsider", document := "directory", permission := "update" }
    , { name := "local-user-write", actor := "user", document := "self", permission := "update" }
    , { name := "revoked-user-write", actor := "user", document := "self", permission := "update" } ]
  model := fun i =>
    { allowed := allowed 5 (discoveryState i.name) i.actor "discovery" i.document i.permission }

structure WriterInput where
  name : String
  creator : String
  namesRuntime : Bool
  deriving ToJson

structure Writer where
  writer : Option String
  deriving ToJson

def writerInputs : List WriterInput :=
  [ { name := "runtime-owner", creator := "runtime", namesRuntime := true }
  , { name := "user-owner", creator := "user", namesRuntime := true }
  , { name := "foreign-owner", creator := "foreign", namesRuntime := true }
  , { name := "other-node", creator := "user", namesRuntime := false } ]

def registryWriter (i : WriterInput) : Writer :=
  { writer := localRegistryWriter i.creator "runtime" "user" i.namesRuntime }

/-- The replicated registry scenario creates rows naming the local Node as the
runtime and as the user, and receives one created by the remote Node. -/
@[conformance_family]
def registryWriterScenario : Family WriterInput Writer where
  inputs := writerInputs.filter (·.name ≠ "other-node")
  model := registryWriter

/-- model-production-gap: the replicated registry scenario creates no row
naming another Node, so no production path observes its writer. -/
@[conformance_family]
def registryWriterUnreached : Family WriterInput Writer where
  inputs := writerInputs.filter (·.name = "other-node")
  model := registryWriter

structure ReaderPolicyInput where
  name : String
  actor : String
  deriving ToJson

structure ReaderPolicy where
  read : Bool
  update : Bool
  manageReader : Bool
  deriving ToJson

def pairingReaderRoute : Relation := ⟨"pairing_observations", "route", "route_reader", .actor "agent"⟩
def pairingReaderTool : Relation := ⟨"pairing_observations", "route", "tool_reader", .actor "agent"⟩
def pairingReaderManager : Relation :=
  ⟨"pairing_observations", "route", "reader_manager", .actor "user"⟩

def pairingReaderRouteOnly : State :=
  apply (apply pairingReaderFixture (.grantAs "user" pairingReaderTool))
    (.revokeAs "user" pairingReaderTool)

def pairingReaderNeither : State :=
  apply pairingReaderRouteOnly (.revokeAs "user" pairingReaderRoute)

def readerPolicyState : String → State
  | "tool-revoked" => pairingReaderRouteOnly
  | "both-revoked" => pairingReaderNeither
  | "tool-only" => apply pairingReaderNeither (.grantAs "user" pairingReaderTool)
  | "manager-revoked" => apply pairingReaderNeither (.revokeAs "runtime" pairingReaderManager)
  | _ => pairingReaderFixture

def readerPolicyInputs : List ReaderPolicyInput :=
  [ { name := "runtime", actor := "runtime" }, { name := "user", actor := "user" }
  , { name := "agent", actor := "agent" }, { name := "sibling", actor := "sibling" }
  , { name := "tool-revoked", actor := "agent" }, { name := "both-revoked", actor := "agent" }
  , { name := "tool-only", actor := "agent" }, { name := "manager-revoked", actor := "user" } ]

def readerPolicy (i : ReaderPolicyInput) : ReaderPolicy :=
  let state := readerPolicyState i.name
  { read := allowed 5 state i.actor "pairing_observations" "route" "read"
    update := allowed 5 state i.actor "pairing_observations" "route" "update"
    manageReader := mayManage state i.actor pairingReaderRoute }

/-- An explicitly revoked reader and a reader whose route and tool grants are
both revoked see the same: nothing. -/
example : ((pairingReaderCases.find? (·.name == "explicit-revocation")).map fun c =>
    (reconcilePairingReader c.before c.entitled c.ownerAuthorized c.committed c.generation).readerGranted) =
    some (readerPolicy { name := "both-revoked", actor := "agent" }).read := by native_decide

structure ReaderRead where
  read : Bool
  deriving ToJson

/-- The applied-pairing scenario reads the route as the reader Agent once the
user grants its route reader, as a sibling Agent, and as the reader Agent while
the user grants and revokes its tool and route readers. -/
@[conformance_family]
def readerPolicyRead : Family ReaderPolicyInput ReaderRead where
  inputs := readerPolicyInputs.filter
    (·.name ∈ ["agent", "sibling", "tool-revoked", "tool-only", "both-revoked"])
  model := fun i => { read := (readerPolicy i).read }

structure ReaderUpdate where
  update : Bool
  deriving ToJson

/-- The applied-pairing scenario has the granted reader Agent update the route
through the authenticated GraphQL server. -/
@[conformance_family]
def readerPolicyUpdate : Family ReaderPolicyInput ReaderUpdate where
  inputs := readerPolicyInputs.filter (·.name = "agent")
  model := fun i => { update := (readerPolicy i).update }

structure ManageReader where
  manageReader : Bool
  deriving ToJson

/-- The applied-pairing scenario revokes the user's reader-manager relation
and then has the user manage the reader. -/
@[conformance_family]
def readerPolicyManage : Family ReaderPolicyInput ManageReader where
  inputs := readerPolicyInputs.filter (·.name = "manager-revoked")
  model := fun i => { manageReader := (readerPolicy i).manageReader }

/-- model-production-gap: the applied-pairing scenario observes only the reads,
the reader Agent's update and the reader management replayed by
`readerPolicyRead`, `readerPolicyUpdate` and `readerPolicyManage`; no
production path observes the runtime's and user's access to the initial route
or any other route update decision. -/
@[conformance_family]
def readerPolicyUnreached : Family ReaderPolicyInput ReaderPolicy where
  inputs := readerPolicyInputs
  model := readerPolicy

structure DeferredInput where
  name : String
  runtime : String
  embeddedRuntime : String
  nativeOwner : Option String
  node : Option String
  nodeRegistered : Bool
  committed : Bool
  beforeParent : Option String
  beforeGranted : Bool
  beforeLinked : Bool
  deriving ToJson

def deferredInputs : List DeferredInput :=
  deferredCases.map fun c =>
    { name := c.name, runtime := c.runtime, embeddedRuntime := c.embeddedRuntime
      nativeOwner := c.nativeOwner, node := c.node, nodeRegistered := c.nodeRegistered
      committed := c.committed, beforeParent := c.before.parent
      beforeGranted := c.before.parentGranted, beforeLinked := c.before.parentLinked }

def deferredAfter (i : DeferredInput) : DeferredOperation :=
  attachDeferredOperation ⟨i.beforeParent, i.beforeGranted, i.beforeLinked⟩ i.runtime
    i.embeddedRuntime i.nativeOwner i.node i.nodeRegistered i.committed

structure Linked where
  linked : Bool
  deriving ToJson

structure Granted where
  granted : Bool
  deriving ToJson

/-- The desired-route materialization scenario writes before the local Node
document exists. -/
@[conformance_family]
def deferredMaterializeLinked : Family DeferredInput Linked where
  inputs := deferredInputs.filter (·.name = "before-node")
  model := fun i => { linked := (deferredAfter i).parentLinked }

/-- The applied-pairing scenario records the observation before the Node
exists and links it once the Node is registered. -/
@[conformance_family]
def deferredAppliedLinked : Family DeferredInput Linked where
  inputs := deferredInputs.filter (·.name ∈ ["before-node", "success"])
  model := fun i => { linked := (deferredAfter i).parentLinked }

/-- The applied-pairing scenario revokes the published parent and repairs
again. -/
@[conformance_family]
def deferredAppliedGranted : Family DeferredInput Granted where
  inputs := deferredInputs.filter (·.name = "revoked-after-publication")
  model := fun i => { granted := (deferredAfter i).parentGranted }

/-- The runtime scenario writes every deferred collection before the Node,
links them, revokes a published parent and writes a document a foreign
owner holds. -/
@[conformance_family]
def deferredRuntimeLinked : Family DeferredInput Linked where
  inputs := deferredInputs.filter (·.name ∈ ["before-node", "success", "foreign-owner"])
  model := fun i => { linked := (deferredAfter i).parentLinked }

/-- The runtime scenario revokes a published parent and recovers. -/
@[conformance_family]
def deferredRuntimeGranted : Family DeferredInput Granted where
  inputs := deferredInputs.filter (·.name = "revoked-after-publication")
  model := fun i => { granted := (deferredAfter i).parentGranted }

structure Deferred where
  parent : Option String
  granted : Bool
  linked : Bool
  deriving ToJson

/-- model-production-gap: the served scenarios observe only the parent link
or grant the families above replay; no production path builds a foreign
embedded runtime, an unregistered Node or a failed commit, or observes the
recorded parent Node. -/
@[conformance_family]
def deferredUnreached : Family DeferredInput Deferred where
  inputs := deferredInputs
  model := fun i =>
    let after := deferredAfter i
    { parent := after.parent, granted := after.parentGranted, linked := after.parentLinked }

structure SettledInput where
  name : String
  livePresent : Bool
  idle : Bool
  resultKnown : Bool
  resultError : Bool
  persistedSettled : Bool
  deriving ToJson

def settledStatus (i : SettledInput) : Option LiveReconcileStatus :=
  if i.livePresent then some ⟨i.idle, i.resultKnown, i.resultError⟩ else none

@[conformance_family]
def readinessSettled : Family SettledInput Bool where
  inputs := settledCases.map fun (name, status, persisted) =>
    { name, livePresent := status.isSome
      idle := status.any (·.idle)
      resultKnown := status.any (·.lastResultKnown)
      resultError := status.any (·.lastResultError)
      persistedSettled := persisted }
  model := fun i => goalRuntimeSettled (settledStatus i) i.persistedSettled

deriving instance ToJson for CollectionBinding

structure BindingInput where
  name : String
  binding : CollectionBinding
  deriving ToJson

@[conformance_family]
def readinessBinding : Family BindingInput Bool where
  inputs := bindingCases.map fun (name, binding) => { name, binding }
  model := fun i => readinessPreflight i.binding

structure RecoveryInput where
  name : String
  pending : Bool
  grantedBefore : Bool
  runtime : String
  nativeOwner : Option String
  recordedParent : Option String
  currentParent : Option String
  grantSucceeded : Bool
  markSucceeded : Bool
  embeddedRuntime : String
  parentRegistered : Bool
  deriving ToJson

def recoveryInputs : List RecoveryInput :=
  recoveryCases.map fun c =>
    { name := c.name, pending := c.pending, grantedBefore := c.parentGranted, runtime := c.runtime
      nativeOwner := c.nativeOwner, recordedParent := c.recordedParent
      currentParent := c.currentParent, grantSucceeded := c.grantSucceeded
      markSucceeded := c.markSucceeded, embeddedRuntime := c.embeddedRuntime
      parentRegistered := c.parentRegistered }

def recoveryAfter (c : RecoveryInput) : ParentPublication :=
  repairReadiness ⟨true, c.grantedBefore, !c.pending⟩ c.runtime c.embeddedRuntime c.parentRegistered
    c.nativeOwner c.recordedParent c.currentParent c.grantSucceeded c.markSucceeded

structure RecoveryOutput where
  parentGranted : Bool
  parentLinked : Bool
  deriving ToJson

structure ParentLinked where
  parentLinked : Bool
  deriving ToJson

/-- The recovery scenario publishes readiness as the embedded runtime over a
row the runtime owns natively, with the Node parent present, replaced,
unregistered or absent, or as a foreign runtime; the user reads the row only
through the parent grant. -/
@[conformance_family]
def readinessRecovery : Family RecoveryInput RecoveryOutput where
  inputs := recoveryInputs.filter (·.name ∈ ["success", "parent-mismatch", "foreign-runtime",
    "unregistered-parent", "already-complete", "parent-missing", "node-unknown"])
  model := fun c => { parentGranted := (recoveryAfter c).parentGranted
                      parentLinked := (recoveryAfter c).parentLinked }

/-- The recovery scenario writes the readiness row with no native owner or as
the user; the user's read of such a row does not measure the parent grant. -/
@[conformance_family]
def readinessRecoveryForeignOwner : Family RecoveryInput ParentLinked where
  inputs := recoveryInputs.filter (·.name ∈ ["owner-unknown", "owner-mismatch"])
  model := fun c => { parentLinked := (recoveryAfter c).parentLinked }

/-- model-production-gap: production cannot inject a failed parent grant or a
failed publication mark, and a readiness row natively owned by an empty runtime
DID cannot be written. -/
@[conformance_family]
def readinessRecoveryUnreached : Family RecoveryInput RecoveryOutput where
  inputs := recoveryInputs.filter (·.name ∈ ["grant-failed", "mark-failed", "retry-mark",
    "runtime-missing"])
  model := fun c => { parentGranted := (recoveryAfter c).parentGranted
                      parentLinked := (recoveryAfter c).parentLinked }

structure Usable where
  usable : Bool
  deriving ToJson

structure ObservationInput where
  name : String
  requestedRuntime : String
  embeddedRuntime : String
  nodeCount : Nat
  readinessCount : Nat
  nodeId : Option String
  nodeOwner : Option String
  readinessOwner : Option String
  recordedParent : Option String
  parentLinked : Bool
  parentPresent : Bool
  deriving ToJson

def observationInputs : List ObservationInput :=
  observationCases.map fun (name, o) =>
    { name, requestedRuntime := o.requestedRuntime, embeddedRuntime := o.embeddedRuntime
      nodeCount := o.nodeCount, readinessCount := o.readinessCount, nodeId := o.nodeId
      nodeOwner := o.nodeOwner, readinessOwner := o.readinessOwner
      recordedParent := o.recordedParent, parentLinked := o.parentLinked
      parentPresent := o.parentPresent }

def observationUsable (i : ObservationInput) : Usable :=
  { usable := readinessObservationUsable ⟨i.requestedRuntime, i.embeddedRuntime, i.nodeCount,
      i.readinessCount, i.nodeId, i.nodeOwner, i.readinessOwner, i.recordedParent,
      i.parentLinked, i.parentPresent⟩ }

/-- The recovery scenario observes, as the embedded runtime, the one Node and
the one readiness row each of its fixtures leaves behind. -/
@[conformance_family]
def readinessObservation : Family ObservationInput Usable where
  inputs := observationInputs.filter (·.name ∈ ["valid", "readiness-unregistered",
    "foreign-readiness", "replaced-parent", "node-unregistered", "revoked-after-publication",
    "pending"])
  model := observationUsable

/-- model-production-gap: the recovery scenario builds exactly one Node and one
readiness row with a recorded parent and observes as the embedded runtime, so no
production path observes a foreign requested runtime, a missing or duplicated
Node or readiness row, or a row with no recorded parent. -/
@[conformance_family]
def readinessObservationUnreached : Family ObservationInput Usable where
  inputs := observationInputs.filter (·.name ∈ ["foreign-runtime", "no-node", "duplicate-node",
    "no-readiness", "duplicate-readiness", "missing-parent"])
  model := observationUsable

structure PartitionInput where
  name : String
  source : String
  peer : String
  owner : String
  deriving ToJson

structure Selected where
  selected : Bool
  deriving ToJson

/-- The applied-pairing scenario persists the runtime's own pairing, a foreign
Node's pairing, a forged pairing naming the runtime under a foreign owner and
the runtime's pairing with another peer, and selects each through the store. -/
@[conformance_family]
def pairingPartition : Family PartitionInput Selected where
  inputs :=
    [ { name := "local", source := "runtime", owner := "runtime", peer := "peer" }
    , { name := "foreign", source := "other", owner := "other", peer := "peer" }
    , { name := "forged", source := "runtime", owner := "other", peer := "peer" }
    , { name := "other-peer", source := "runtime", owner := "runtime", peer := "other-peer" } ]
  model := fun i => { selected := pairingInPartition "runtime" i.source i.owner i.peer "peer" }

structure PairingReaderInput where
  name : String
  beforeGeneration : Option Nat
  beforeGranted : Bool
  entitled : Bool
  ownerAuthorized : Bool
  committed : Bool
  offeredGeneration : Nat
  deriving ToJson

def pairingReaderInputs : List PairingReaderInput :=
  pairingReaderCases.map fun c =>
    { name := c.name, beforeGeneration := c.before.lastGrantedGeneration
      beforeGranted := c.before.readerGranted, entitled := c.entitled
      ownerAuthorized := c.ownerAuthorized, committed := c.committed, offeredGeneration := c.generation }

def pairingReaderAfter (i : PairingReaderInput) : PairingReaderPublication :=
  reconcilePairingReader ⟨i.beforeGeneration, i.beforeGranted⟩ i.entitled i.ownerAuthorized
    i.committed i.offeredGeneration

structure ReaderGranted where
  granted : Bool
  deriving ToJson

/-- The applied-pairing scenario grants the reader Agent its route reader,
aborts a staged grant and a staged revocation, and revokes the reader. -/
@[conformance_family]
def pairingReaderGrant : Family PairingReaderInput ReaderGranted where
  inputs := pairingReaderInputs.filter (·.name ∈ ["initial-grant", "aborted-grant",
    "aborted-revoke", "explicit-revocation"])
  model := fun i => { granted := (pairingReaderAfter i).readerGranted }

structure ReaderPublication where
  generation : Option Nat
  granted : Bool
  deriving ToJson

/-- model-production-gap: production records no owner-authorized reader
generation, so no production path observes the last granted generation, an
entitlement withdrawn without a revocation, or a new or unauthorized
generation. -/
@[conformance_family]
def pairingReaderUnreached : Family PairingReaderInput ReaderPublication where
  inputs := pairingReaderInputs
  model := fun i =>
    { generation := (pairingReaderAfter i).lastGrantedGeneration
      granted := (pairingReaderAfter i).readerGranted }

structure EnrollmentPolicyInput where
  name : String
  actor : String
  document : String
  permission : String
  deriving ToJson

def thinEnrollmentState : State :=
  publishEnrollmentRequest { objects := [], relations := [], enrolled := [] } "client" "admin"
    "request" none true false true true

def enrollmentPolicyState : String → State
  | "candidate-request-write" | "admin-request-read" | "admin-request-write"
  | "user-request-before-parent" => enrollmentRequestFixture
  | "user-request-after-parent" | "outsider-request" => enrollmentRequestWithParent
  | "revoked-admin" => apply enrollmentRequestWithParent (.revokeAs "candidate"
      ⟨"enrollment_records", "request", "reader", .actor "admin"⟩)
  | "admin-terminal-write" | "candidate-terminal-read" | "candidate-terminal-write"
  | "user-terminal-read" | "outsider-terminal" => enrollmentTerminalFixture
  | _ => thinEnrollmentState

def enrollmentPolicyInputs : List EnrollmentPolicyInput :=
  [ { name := "thin-client-write", actor := "client", document := "request", permission := "update" }
  , { name := "thin-admin-read", actor := "admin", document := "request", permission := "read" }
  , { name := "thin-admin-write", actor := "admin", document := "request", permission := "update" }
  , { name := "thin-user-read", actor := "user", document := "request", permission := "read" }
  , { name := "thin-agent-read", actor := "agent", document := "request", permission := "read" }
  , { name := "thin-outsider-read", actor := "outsider", document := "request", permission := "read" }
  , { name := "candidate-request-write", actor := "candidate", document := "request", permission := "update" }
  , { name := "admin-request-read", actor := "admin", document := "request", permission := "read" }
  , { name := "admin-request-write", actor := "admin", document := "request", permission := "update" }
  , { name := "user-request-before-parent", actor := "user", document := "request", permission := "read" }
  , { name := "user-request-after-parent", actor := "user", document := "request", permission := "read" }
  , { name := "outsider-request", actor := "outsider", document := "request", permission := "read" }
  , { name := "revoked-admin", actor := "admin", document := "request", permission := "read" }
  , { name := "admin-terminal-write", actor := "admin", document := "terminal", permission := "update" }
  , { name := "candidate-terminal-read", actor := "candidate", document := "terminal", permission := "read" }
  , { name := "candidate-terminal-write", actor := "candidate", document := "terminal", permission := "update" }
  , { name := "user-terminal-read", actor := "user", document := "terminal", permission := "read" }
  , { name := "outsider-terminal", actor := "outsider", document := "terminal", permission := "read" } ]

def enrollmentPolicyAllowed (i : EnrollmentPolicyInput) : Allowed :=
  { allowed := allowed 5 (enrollmentPolicyState i.name) i.actor "enrollment_records" i.document
      i.permission }

/-- The thin-client scenario writes its own enrollment request with no local
Node and asks native ACP and the authenticated GraphQL server for each actor's
access to it. -/
@[conformance_family]
def enrollmentPolicyThin : Family EnrollmentPolicyInput Allowed where
  inputs := enrollmentPolicyInputs.filter (·.name.startsWith "thin-")
  model := enrollmentPolicyAllowed

/-- model-production-gap: the thin-client scenario builds only the request a
client writes before it has a Node; no production scenario builds the admin's
received request, its parent, the revoked admin reader or the terminal
decision. -/
@[conformance_family]
def enrollmentPolicyUnreached : Family EnrollmentPolicyInput Allowed where
  inputs := enrollmentPolicyInputs.filter fun i => !i.name.startsWith "thin-"
  model := enrollmentPolicyAllowed

structure ReplicaInput where
  name : String
  deriving ToJson

def replicaPending : State :=
  replicaGrant privateReplicaFixture "enrollment_records" "remote" "runtime" "local-node" "user"
    true true true false false

def replicaLinked : State :=
  replicaLinkPending replicaPending "enrollment_records" "remote" "runtime" "local-node" "user"
    true true true

def replicaRevoked : State :=
  apply replicaLinked (.revoke ⟨"enrollment_records", "remote", "parent", .object "nodes" "local-node"⟩)

def replicaState : String → State
  | "pending" => replicaPending
  | "linked" => replicaLinked
  | "revoked" => replicaRevoked
  | "after-repair" => replicaLinkPending replicaRevoked "enrollment_records" "remote" "runtime"
      "local-node" "user" true true true
  | _ => privateReplicaFixture

def replicaInputs : List ReplicaInput :=
  [{ name := "before" }, { name := "pending" }, { name := "linked" }, { name := "revoked" },
   { name := "after-repair" }]

def replicaRead (i : ReplicaInput) (actor collection document : String) : Bool :=
  allowed 5 (replicaState i.name) actor collection document "read"

structure ReplicaReads where
  runtimeRead : Bool
  userRead : Bool
  agentRead : Bool
  outsiderRead : Bool
  deriving ToJson

def replicaReads (i : ReplicaInput) : ReplicaReads :=
  { runtimeRead := replicaRead i "runtime" "enrollment_records" "remote"
    userRead := replicaRead i "user" "enrollment_records" "remote"
    agentRead := replicaRead i "agent" "enrollment_records" "remote"
    outsiderRead := replicaRead i "outsider" "enrollment_records" "remote" }

/-- The replicated enrollment scenario reads the received request through the
receiver's runtime and authenticated GraphQL server once it is pending, once
the local Node links it and once a revoked parent is repaired. -/
@[conformance_family]
def replicaGrantReads : Family ReplicaInput ReplicaReads where
  inputs := replicaInputs.filter (·.name ∈ ["pending", "linked", "after-repair"])
  model := replicaReads

structure UserWrite where
  userWrite : Bool
  deriving ToJson

/-- The replicated enrollment scenario has the user update the linked
request. -/
@[conformance_family]
def replicaGrantUserWrite : Family ReplicaInput UserWrite where
  inputs := replicaInputs.filter (·.name = "linked")
  model := fun i => { userWrite := allowed 5 (replicaState i.name) "user" "enrollment_records" "remote" "update" }

structure AppRead where
  appRead : Bool
  deriving ToJson

/-- The replicated enrollment scenario creates a private application session
only after the revoked parent is repaired. -/
@[conformance_family]
def replicaGrantAppRead : Family ReplicaInput AppRead where
  inputs := replicaInputs.filter (·.name = "after-repair")
  model := fun i => { appRead := replicaRead i "runtime" "children" "application" }

structure ReplicaPending where
  pending : Bool
  deriving ToJson

def replicaPendingRelation (i : ReplicaInput) : ReplicaPending :=
  { pending := (replicaState i.name).relations.contains
      ⟨"enrollment_records", "remote", "pending_parent", .actor "runtime"⟩ }

/-- The installed runtime projector writes a transport document before the
local Node exists, links it once the Node is written, and keeps a revoked
parent revoked when the document is next written. -/
@[conformance_family]
def replicaGrantPending : Family ReplicaInput ReplicaPending where
  inputs := replicaInputs.filter (·.name ∈ ["pending", "linked", "revoked"])
  model := replicaPendingRelation

structure ReplicaGrant where
  runtimeRead : Bool
  userRead : Bool
  userWrite : Bool
  agentRead : Bool
  outsiderRead : Bool
  appRead : Bool
  pending : Bool
  deriving ToJson

/-- model-production-gap: no production scenario observes the received request
before any grant, or reads it between the parent's revocation and its repair;
the user's write is attempted only on the linked request, the application
session exists only after the repair, and the pending relation is observed only
by the runtime projector, which repairs no revoked parent. -/
@[conformance_family]
def replicaGrantUnreached : Family ReplicaInput ReplicaGrant where
  inputs := replicaInputs
  model := fun i =>
    { runtimeRead := (replicaReads i).runtimeRead, userRead := (replicaReads i).userRead
      userWrite := allowed 5 (replicaState i.name) "user" "enrollment_records" "remote" "update"
      agentRead := (replicaReads i).agentRead, outsiderRead := (replicaReads i).outsiderRead
      appRead := replicaRead i "runtime" "children" "application"
      pending := (replicaPendingRelation i).pending }

end Conformance.IdentityOperations
