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
  , { name := "unheld", actor := "client", «local» := "client", target := "remote", held := false } ]

def publisherAllowed (i : PublisherInput) : Allowed :=
  { allowed := desiredRoutePublisher i.actor i.«local» i.target i.held }

/-- The materialization scenario writes as the held local Node, or as a
remote identity whose signer the store does not hold, toward the local or a
remote Node. A store holds only its own Node's signer, so a held signer is the
local Node's; `desired_route_rejects_foreign_or_unheld_author` pins the foreign
author. -/
@[conformance_family]
def desiredPublisher : Family PublisherInput Allowed where
  inputs := publisherInputs
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
runtime and as the user, receives one created by the remote Node, and has the
user create one naming another Node. -/
@[conformance_family]
def registryWriterScenario : Family WriterInput Writer where
  inputs := writerInputs
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
    setPairingReader c.granted c.grant c.committed) =
    some (readerPolicy { name := "both-revoked", actor := "agent" }).read := by native_decide

/-- The applied-pairing scenario reads, updates and manages the route reader as
the runtime, the user, the reader Agent and a sibling Agent once the user grants
the route reader; as the reader Agent while the user grants and revokes its tool
and route readers; and as the user once the runtime revokes its reader
management. -/
@[conformance_family]
def readerPolicyAccess : Family ReaderPolicyInput ReaderPolicy where
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

structure Deferred where
  parent : Option String
  granted : Bool
  linked : Bool
  deriving ToJson

/-- The deferred-enrollment scenario writes every deferred collection before
the Node, decides each pending row against an unregistered Node, for a foreign
requested runtime and in an aborted transaction, links them once the user's Node
exists, revokes a published parent and recovers, and refuses a row its foreign
owner parented. -/
@[conformance_family]
def deferred : Family DeferredInput Deferred where
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
unregistered or absent, as a foreign runtime, with the publication mark
aborted and retried; the user reads the row only through the parent grant. -/
@[conformance_family]
def readinessRecovery : Family RecoveryInput RecoveryOutput where
  inputs := recoveryInputs.filter
    (·.name ∉ ["owner-unknown", "owner-mismatch", "grant-failed", "retry-grant-failed",
      "mark-failed", "retry-mark"])
  model := fun c => { parentGranted := (recoveryAfter c).parentGranted
                      parentLinked := (recoveryAfter c).parentLinked }

/-- The recovery scenario writes the readiness row with no native owner or as
the user; the user's read of such a row does not measure the parent grant. -/
@[conformance_family]
def readinessRecoveryForeignOwner : Family RecoveryInput ParentLinked where
  inputs := recoveryInputs.filter (·.name ∈ ["owner-unknown", "owner-mismatch"])
  model := fun c => { parentLinked := (recoveryAfter c).parentLinked }

/-- The parent grant is a native relationship change and the publication mark
is an auto-commit write after it; the store's fault seams fail only mutations
inside an apply transaction, so no fixture fails the grant or the mark alone. -/
@[conformance_family]
def readinessRecoveryUnreached : Family RecoveryInput RecoveryOutput where
  inputs := recoveryInputs.filter
    (·.name ∈ ["grant-failed", "retry-grant-failed", "mark-failed", "retry-mark"])
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

/-- The recovery scenario observes the readiness row each of its fixtures
leaves behind, as the embedded runtime or as a foreign one, with the one Node it
registers, with no Node when the fixture records an unknown parent, and with
the row deleted. -/
@[conformance_family]
def readinessObservation : Family ObservationInput Usable where
  inputs := observationInputs
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
  granted : Bool
  grant : Bool
  committed : Bool
  deriving ToJson

structure ReaderGranted where
  granted : Bool
  deriving ToJson

/-- The applied-pairing scenario grants the reader Agent its route reader,
aborts a staged grant and a staged revocation, and revokes the reader. -/
@[conformance_family]
def pairingReaderGrant : Family PairingReaderInput ReaderGranted where
  inputs := pairingReaderCases.map fun c =>
    { name := c.name, granted := c.granted, grant := c.grant, committed := c.committed }
  model := fun i => { granted := setPairingReader i.granted i.grant i.committed }

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

/-- The replicated enrollment scenario reads and updates the request on the
candidate's node as a thin client writes it, before it has a Node; on the
admin's node once received, before and after its parent link and with the
admin's reader revoked; and the terminal decision the admin's runtime publishes
to the candidate under its Node. -/
@[conformance_family]
def enrollmentPolicy : Family EnrollmentPolicyInput Allowed where
  inputs := enrollmentPolicyInputs
  model := enrollmentPolicyAllowed

structure ReplicaInput where
  name : String
  deriving ToJson

def replicaPending : State :=
  replicaGrant privateReplicaFixture "foreign" "enrollment_records" "remote" "runtime" "local-node" "user"
    true true true false false

def replicaLinked : State :=
  replicaLinkPending replicaPending "foreign" "enrollment_records" "remote" "runtime" "local-node" "user"
    true true true

def replicaRevoked : State :=
  apply replicaLinked (.revoke ⟨"enrollment_records", "remote", "parent", .object "nodes" "local-node"⟩)

def replicaState : String → State
  | "pending" => replicaPending
  | "linked" => replicaLinked
  | "revoked" => replicaRevoked
  | "after-repair" => replicaLinkPending replicaRevoked "foreign" "enrollment_records" "remote" "runtime"
      "local-node" "user" true true true
  | _ => privateReplicaFixture

def replicaInputs : List ReplicaInput :=
  [{ name := "before" }, { name := "pending" }, { name := "linked" }, { name := "revoked" },
   { name := "after-repair" }]

def replicaRead (i : ReplicaInput) (actor collection document : String) : Bool :=
  allowed 5 (replicaState i.name) actor collection document "read"

/-- `runtime` in this scenario is the receiving node's own key, which reads
past document ACP (`Household.nativeRead`); the other actors read what document
ACP grants. -/
def replicaHostRead (i : ReplicaInput) (collection document : String) : Bool :=
  nativeRead true 5 (replicaState i.name) "runtime" collection document

structure ReplicaGrant where
  runtimeRead : Bool
  userRead : Bool
  userWrite : Bool
  agentRead : Bool
  outsiderRead : Bool
  appRead : Bool
  pending : Bool
  deriving ToJson

/-- The replicated enrollment scenario receives a request before any grant,
publishes it while the local Node is absent, links it once the Node exists,
revokes its parent and writes it again. In each state it reads the request as
the receiver's runtime, the user, an Agent and an outsider, attempts the user's
update, reads a private application session as the hosting node key, and reads
the runtime's pending-parent relation. -/
@[conformance_family]
def replicaGrantScenario : Family ReplicaInput ReplicaGrant where
  inputs := replicaInputs
  model := fun i =>
    { runtimeRead := replicaHostRead i "enrollment_records" "remote"
      userRead := replicaRead i "user" "enrollment_records" "remote"
      userWrite := allowed 5 (replicaState i.name) "user" "enrollment_records" "remote" "update"
      agentRead := replicaRead i "agent" "enrollment_records" "remote"
      outsiderRead := replicaRead i "outsider" "enrollment_records" "remote"
      appRead := replicaHostRead i "children" "application"
      pending := (replicaState i.name).pendingParents.contains ("enrollment_records", "remote", "runtime") }

end Conformance.IdentityOperations
