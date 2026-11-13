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

end Conformance.IdentityOperations
