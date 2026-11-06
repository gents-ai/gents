import Proofs.Identity.Household

namespace Identity.Operations

open Identity.Household

/-- A runtime-authored operational document has a separate publication step:
ACP1 creates it for the authenticated runtime before the runtime attaches its
Node parent. A field containing a Node DID does not create this relationship.
The parent grant is a detached operation and may fail or be delayed. -/
def beforeParent (user runtime reader : DID) : State :=
  directCreate
    { objects := [⟨"nodes", "home", user⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor reader⟩],
      enrolled := [] }
    runtime "operations" "readiness" true true

def afterParent (user runtime reader : DID) : State :=
  apply (beforeParent user runtime reader)
    (.grantAs runtime ⟨"operations", "readiness", "parent", .object "nodes" "home"⟩)

theorem operational_parent_grant_changes_user_visibility :
    (allowed 5 (beforeParent "user" "runtime" "reader") "user" "operations" "readiness" "read",
     allowed 5 (afterParent "user" "runtime" "reader") "user" "operations" "readiness" "read") = (false, true) := by
  native_decide

theorem parent_node_runtime_reader_inherits_operational_read :
    allowed 5 (afterParent "user" "runtime" "reader") "reader" "operations" "readiness" "read" = true := by
  native_decide

theorem runtime_owns_operation_user_reads_but_cannot_update :
    (allowed 5 (afterParent "user" "runtime" "reader") "runtime" "operations" "readiness" "update",
     allowed 5 (afterParent "user" "runtime" "reader") "user" "operations" "readiness" "read",
     allowed 5 (afterParent "user" "runtime" "reader") "user" "operations" "readiness" "update",
     allowed 5 (afterParent "user" "runtime" "reader") "agent" "operations" "readiness" "read") =
      (true, true, false, false) := by
  native_decide

/-- A runtime operation is published together with its Node relation. A
failed relationship or transaction commit leaves neither change visible.
Existing documents are not silently reparented: an owner may have revoked the
relationship after an earlier publication. -/
def publishOperation (state : State) (runtime : DID) (id node : String)
    (createSucceeded relationshipSucceeded commitSucceeded : Bool) : State :=
  if createSucceeded && relationshipSucceeded && commitSucceeded then
    apply (directCreate state runtime "operations" id true true)
      (.grantAs runtime ⟨"operations", id, "parent", .object "nodes" node⟩)
  else state

theorem failed_operation_publication_has_no_effect (state : State) (runtime : DID)
    (id node : String) (created committed : Bool) :
    publishOperation state runtime id node created false committed = state := by
  simp [publishOperation]

/-- Only the pairing owner or an explicitly granted reader manager may manage
this physical observation's readers. The reader relation grants no writes and
is independent of every other operational document. Publication of a standing
reader must be driven by its actual entitlement owner, atomically fenced with
its revocation; this resource alone does not establish that lifecycle. -/
def pairingReaderFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩,
      ⟨"pairing_observations", "route", "runtime"⟩,
      ⟨"pairing_observations", "other-route", "runtime"⟩,
      ⟨"operations", "secret-operation", "runtime"⟩],
    relations := [⟨"pairing_observations", "route", "parent", .object "nodes" "node"⟩,
      ⟨"pairing_observations", "route", "reader_manager", .actor "user"⟩,
      ⟨"pairing_observations", "route", "route_reader", .actor "agent"⟩], enrolled := [] }

theorem pairing_reader_is_document_scoped_and_read_only :
    (allowed 5 pairingReaderFixture "runtime" "pairing_observations" "route" "update",
     allowed 5 pairingReaderFixture "user" "pairing_observations" "route" "read",
     allowed 5 pairingReaderFixture "agent" "pairing_observations" "route" "read",
     allowed 5 pairingReaderFixture "agent" "pairing_observations" "route" "update",
     allowed 5 pairingReaderFixture "agent" "pairing_observations" "other-route" "read",
     allowed 5 pairingReaderFixture "agent" "operations" "secret-operation" "read",
     allowed 5 pairingReaderFixture "sibling" "pairing_observations" "route" "read") =
      (true, true, true, false, false, false, false) := by native_decide

theorem pairing_reader_manager_can_revoke_without_writing :
    let revoked := apply pairingReaderFixture (.revokeAs "user"
      ⟨"pairing_observations", "route", "route_reader", .actor "agent"⟩)
    (allowed 5 pairingReaderFixture "user" "pairing_observations" "route" "update",
     allowed 5 revoked "agent" "pairing_observations" "route" "read") = (false, false) := by
  native_decide

/-- Route admission and tool observation are independent explicit grants.
Revoking one does not remove another independently authorized reason to read;
neither grant is inferred from the selected Tools flag. -/
theorem pairing_reader_reasons_are_independent :
    let both := apply pairingReaderFixture (.grantAs "user"
      ⟨"pairing_observations", "route", "tool_reader", .actor "agent"⟩)
    let toolRevoked := apply both (.revokeAs "user"
      ⟨"pairing_observations", "route", "tool_reader", .actor "agent"⟩)
    let bothRevoked := apply toolRevoked (.revokeAs "user"
      ⟨"pairing_observations", "route", "route_reader", .actor "agent"⟩)
    (allowed 5 toolRevoked "agent" "pairing_observations" "route" "read",
     allowed 5 bothRevoked "agent" "pairing_observations" "route" "read") = (true, false) := by
  native_decide

/-- ACP evaluates relationship tuples, not arbitrary mutable Tools fields.
An authenticated raw GraphQL update can disable the P2P tool without executing
Gents' desired-state publication. A derived reader relation therefore needs a
native transaction constraint or hook covering every write path; observing the
flag in a reconciler alone cannot maintain the advertised raw-GraphQL boundary. -/
structure ToolReaderObservation where
  acp : State
  p2pEnabled : Bool

def rawDisableTool (observation : ToolReaderObservation) (actor : DID)
    (committed : Bool) : ToolReaderObservation :=
  if allowed 5 observation.acp actor "configuration" "tools" "update" && committed then
    { observation with p2pEnabled := false }
  else observation

def toolReaderObservation : ToolReaderObservation :=
  { acp := { pairingReaderFixture with objects :=
      ⟨"configuration", "tools", "user"⟩ :: pairingReaderFixture.objects },
    p2pEnabled := true }

theorem raw_tool_disable_alone_does_not_revoke_reader :
    let after := rawDisableTool toolReaderObservation "user" true
    (after.p2pEnabled,
     allowed 5 after.acp "agent" "pairing_observations" "route" "read") = (false, true) := by
  native_decide

/-- A thin client authors enrollment requests using its own held peer key and
need not have a Node configuration document. An absent visible Node leaves its
request owned by that client, with only the verified server's explicit reader
grant. A runtime with a visible Node links that exact registered parent in the
same transaction. Neither path invents a Node or restores a revoked grant. -/
def publishEnrollmentRequest (state : State) (candidate admin id : String)
    (parent : Option String) (created parentGranted readerGranted committed : Bool) : State :=
  if created && readerGranted && committed && (parent.isNone || parentGranted) then
    let created := directCreate state candidate "enrollment_records" id true true
    let parented := match parent with
      | none => created
      | some node => apply created (.grantAs candidate
          ⟨"enrollment_records", id, "parent", .object "nodes" node⟩)
    apply parented (.grantAs candidate ⟨"enrollment_records", id, "reader", .actor admin⟩)
  else state

theorem thin_client_request_remains_creator_owned_and_private :
    let initial : State := { objects := [], relations := [], enrolled := [] }
    let published := publishEnrollmentRequest initial "client" "admin" "request" none
      true false true true
    (allowed 5 published "client" "enrollment_records" "request" "update",
     allowed 5 published "admin" "enrollment_records" "request" "read",
     allowed 5 published "admin" "enrollment_records" "request" "update",
     allowed 5 published "user" "enrollment_records" "request" "read",
     allowed 5 published "agent" "enrollment_records" "request" "read",
     allowed 5 published "outsider" "enrollment_records" "request" "read") =
      (true, true, false, false, false, false) := by native_decide

theorem parent_failure_aborts_runtime_request (state : State) (candidate admin id node : String)
    (created reader committed : Bool) :
    publishEnrollmentRequest state candidate admin id (some node) created false reader committed = state := by
  simp [publishEnrollmentRequest]

theorem reader_failure_aborts_client_request (state : State) (candidate admin id : String)
    (created parent committed : Bool) :
    publishEnrollmentRequest state candidate admin id none created parent false committed = state := by
  simp [publishEnrollmentRequest]

/-- Enrollment requests contain the signed offer token and client nonce. The
candidate runtime is their native owner; the exact admin runtime is an explicit
reader. A parent is a native relationship to a physical Node, not a DID field
or a consequence of enrollment. Foreign relationships are not assumed to
replicate. Terminal decisions, revisions and receipts reverse the creator and
reader roles. A candidate User is a separate identity and must be authenticated
independently before it is granted access to a decision. -/
def enrollmentRequestFixture : State :=
  { objects := [⟨"nodes", "candidate-node", "user"⟩,
      ⟨"enrollment_records", "request", "candidate"⟩],
    relations := [⟨"enrollment_records", "request", "reader", .actor "admin"⟩],
    enrolled := [] }

def enrollmentRequestWithParent : State :=
  apply enrollmentRequestFixture (.grantAs "candidate"
    ⟨"enrollment_records", "request", "parent", .object "nodes" "candidate-node"⟩)

theorem request_reader_is_exact_and_read_only :
    (allowed 5 enrollmentRequestFixture "candidate" "enrollment_records" "request" "update",
     allowed 5 enrollmentRequestFixture "admin" "enrollment_records" "request" "read",
     allowed 5 enrollmentRequestFixture "admin" "enrollment_records" "request" "update",
     allowed 5 enrollmentRequestFixture "user" "enrollment_records" "request" "read",
     allowed 5 enrollmentRequestFixture "sibling" "enrollment_records" "request" "read",
     allowed 5 enrollmentRequestFixture "outsider" "enrollment_records" "request" "read") =
      (true, true, false, false, false, false) := by native_decide

theorem user_reaches_request_only_after_exact_node_parent :
    (allowed 5 enrollmentRequestFixture "user" "enrollment_records" "request" "read",
     allowed 5 enrollmentRequestWithParent "user" "enrollment_records" "request" "read") =
      (false, true) := by native_decide

theorem enrollment_does_not_reconstruct_revoked_request_reader :
    let revoked := apply enrollmentRequestWithParent (.revokeAs "candidate"
      ⟨"enrollment_records", "request", "reader", .actor "admin"⟩)
    allowed 5 revoked "admin" "enrollment_records" "request" "read" = false := by native_decide

def enrollmentTerminalFixture : State :=
  { objects := [⟨"nodes", "admin-node", "user"⟩,
      ⟨"enrollment_records", "terminal", "admin"⟩],
    relations := [⟨"enrollment_records", "terminal", "reader", .actor "candidate"⟩,
      ⟨"enrollment_records", "terminal", "parent", .object "nodes" "admin-node"⟩],
    enrolled := [] }

theorem terminal_reads_are_scoped_to_candidate_and_node_owner :
    (allowed 5 enrollmentTerminalFixture "admin" "enrollment_records" "terminal" "update",
     allowed 5 enrollmentTerminalFixture "candidate" "enrollment_records" "terminal" "read",
     allowed 5 enrollmentTerminalFixture "candidate" "enrollment_records" "terminal" "update",
     allowed 5 enrollmentTerminalFixture "user" "enrollment_records" "terminal" "read",
     allowed 5 enrollmentTerminalFixture "sibling" "enrollment_records" "terminal" "read",
     allowed 5 enrollmentTerminalFixture "outsider" "enrollment_records" "terminal" "read") =
      (true, true, false, true, false, false) := by native_decide

/-- Enrollment records are private; registry and directory metadata are separately published.
A native reader grant is per physical document and does not imply permission on
any application document. This classification does not publish a binding: each
writer and receiver still needs the authenticated publication path. -/
def networkProtocolResource : String → Option String
  | "Network" => some "enrollment_records"
  | "NetworkEnrollmentRequest" => some "enrollment_records"
  | "NetworkEnrollmentDecision" => some "enrollment_records"
  | "NetworkEnrollmentRouteReceipt" => some "enrollment_records"
  | "NetworkAuthorizationRevision" => some "enrollment_records"
  | "NodeDirectoryEntry" => some "scoped_discovery"
  | "PeerRegistry" => some "discovery"
  | _ => none

/-- PeerRegistry visibility is an explicit wildcard relationship on each published
registry document, not an automated wildcard grant on an enrollment record.
NodeDirectoryEntry is scoped to attached clients rather than assumed public.
An authorized native owner or reader manager may intentionally share a private
record with the wildcard; ordinary ungranted actors cannot. Native ACP
relationships are store-local, so a receiver cannot infer this tuple from a
replicated document; publication on each store needs native Node authority. -/
def publicDiscoveryFixture : State :=
  { objects := [⟨"discovery", "directory", "runtime"⟩,
      ⟨"enrollment_records", "request", "runtime"⟩],
    relations := [⟨"discovery", "directory", "reader", .wildcard⟩], enrolled := [] }

theorem public_discovery_does_not_publish_enrollment :
    (allowed 5 publicDiscoveryFixture "outsider" "discovery" "directory" "read",
     allowed 5 publicDiscoveryFixture "outsider" "discovery" "directory" "update",
     allowed 5 publicDiscoveryFixture "outsider" "enrollment_records" "request" "read") =
      (true, false, false) := by native_decide

/-- PeerRegistry has two actual local writers: the held runtime's heartbeat and
the Node-owning User's registration. Only a row naming that runtime whose
registered creator is one of those two may grant the other writer. A foreign
creator or a row naming another Node cannot grant a local writer. -/
def localRegistryWriter (creator runtime user : DID) (namesRuntime : Bool) : Option DID :=
  if !namesRuntime then none else if creator == runtime then some user
  else if creator == user then some runtime else none

theorem local_registry_writer_is_bound_to_creator_and_node :
    [localRegistryWriter "runtime" "runtime" "user" true,
     localRegistryWriter "user" "runtime" "user" true,
     localRegistryWriter "foreign" "runtime" "user" true,
     localRegistryWriter "user" "runtime" "user" false] =
      [some "user", some "runtime", none, none] := by decide

/-- Native node-administrator publication is local to the receiving store. Its
actor is the configured Node, and native NAC bypass plus relation-add authority,
the bound schema and a verified registered document must all hold in the same
transaction. These grants are specific to the document and do not grant access
on application records or follow merely from peer admission. The local marker
is an ACP relationship, not a replicated document field. -/
def replicaGrant (state : State) (resource id runtime node user : String)
    (authorized registered committed nodeExists publicRegistry : Bool) : State :=
  let marker : Relation := ⟨resource, id, "local_publication", .actor runtime⟩
  if !(authorized && registered && committed) || state.relations.contains marker then state
  else
    let marked := apply state (.grant marker)
    let reader := apply marked (.grant ⟨resource, id, "reader", .actor runtime⟩)
    let visible := if publicRegistry && resource == "discovery" then
      apply reader (.grant ⟨resource, id, "reader", .wildcard⟩) else reader
    if nodeExists then
      let linked := apply visible (.grant ⟨resource, id, "parent", .object "nodes" node⟩)
      apply linked (.grant ⟨resource, id, "reader_manager", .actor user⟩)
    else apply visible (.grant ⟨resource, id, "pending_parent", .actor runtime⟩)

/-- The pending marker is consumed with the Node parent and reader manager in
the same native transaction. Once consumed, reconciliation cannot restore a
relation that was later revoked. The attachment names the physical Node document;
a replacement Node requires an explicit administrator reattachment. -/
def replicaLinkPending (state : State) (resource id runtime node user : String)
    (authorized nodeExists committed : Bool) : State :=
  let marker : Relation := ⟨resource, id, "local_publication", .actor runtime⟩
  let pending : Relation := ⟨resource, id, "pending_parent", .actor runtime⟩
  if authorized && nodeExists && committed && state.relations.contains marker &&
      state.relations.contains pending then
    let linked := apply state (.grant ⟨resource, id, "parent", .object "nodes" node⟩)
    let managed := apply linked (.grant ⟨resource, id, "reader_manager", .actor user⟩)
    apply managed (.revoke pending)
  else state

def privateReplicaFixture : State :=
  { objects := [⟨"nodes", "local-node", "user"⟩,
      ⟨"enrollment_records", "remote", "foreign"⟩,
      ⟨"children", "application", "foreign"⟩], relations := [], enrolled := [] }

theorem native_local_replica_grant_is_narrow :
    let state := replicaGrant privateReplicaFixture "enrollment_records" "remote" "runtime"
      "local-node" "user" true true true true false
    (allowed 5 state "runtime" "enrollment_records" "remote" "read",
     allowed 5 state "user" "enrollment_records" "remote" "read",
     allowed 5 state "user" "enrollment_records" "remote" "update",
     allowed 5 state "agent" "enrollment_records" "remote" "read",
     allowed 5 state "runtime" "children" "application" "read") =
      (true, true, false, false, false) := by native_decide

theorem replica_parent_repair_does_not_restore_revocation :
    let pending := replicaGrant privateReplicaFixture "enrollment_records" "remote" "runtime"
      "local-node" "user" true true true false false
    let linked := replicaLinkPending pending "enrollment_records" "remote" "runtime"
      "local-node" "user" true true true
    let revoked := apply linked (.revoke ⟨"enrollment_records", "remote", "parent",
      .object "nodes" "local-node"⟩)
    allowed 5 (replicaLinkPending revoked "enrollment_records" "remote" "runtime"
      "local-node" "user" true true true) "user" "enrollment_records" "remote" "read" = false := by
  native_decide

/-- A thin client may administer its own native store with its held peer DID
without having a Node configuration document. On receipt of a verified foreign
transport record, native NAC grants that exact client a local reader relation;
the foreign creator remains the owner and the parent remains pending. This is
local store authority, not peer enrollment or access to application records. -/
def clientTransportFixture : State :=
  { objects := [⟨"enrollment_records", "decision", "server"⟩,
      ⟨"children", "application", "server"⟩], relations := [], enrolled := [] }

def clientTransportPublished : State :=
  replicaGrant clientTransportFixture "enrollment_records" "decision" "client"
    "client-node" "user" true true true false false

theorem client_receives_only_its_local_transport_reader :
    (allowed 5 clientTransportPublished "server" "enrollment_records" "decision" "update",
     allowed 5 clientTransportPublished "client" "enrollment_records" "decision" "read",
     allowed 5 clientTransportPublished "client" "enrollment_records" "decision" "update",
     allowed 5 clientTransportPublished "user" "enrollment_records" "decision" "read",
     allowed 5 clientTransportPublished "agent" "enrollment_records" "decision" "read",
     allowed 5 clientTransportPublished "outsider" "enrollment_records" "decision" "read",
     allowed 5 clientTransportPublished "client" "children" "application" "read") =
      (true, true, false, false, false, false, false) := by native_decide

theorem client_without_native_authority_cannot_publish :
    replicaGrant clientTransportFixture "enrollment_records" "decision" "client"
      "client-node" "user" false true true false false = clientTransportFixture := by
  simp [replicaGrant]

theorem native_replica_requires_authority_and_registration :
    (replicaGrant privateReplicaFixture "enrollment_records" "remote" "runtime"
      "local-node" "user" false true true true false,
     replicaGrant privateReplicaFixture "enrollment_records" "remote" "runtime"
      "local-node" "user" true false true true false) =
      (privateReplicaFixture, privateReplicaFixture) := by simp [replicaGrant]

/-- Document creation and all requested native grants commit together. The
recipient DID must come from the authenticated protocol; naming an enrolled
peer alone neither creates nor restores a reader grant. -/
def publishNetworkRecord (state : State) (creator recipient : DID) (id node : String)
    (created parentGranted readerGranted committed : Bool) : State :=
  if created && parentGranted && readerGranted && committed then
    let created := directCreate state creator "enrollment_records" id true true
    let parent := apply created
      (.grantAs creator ⟨"enrollment_records", id, "parent", .object "nodes" node⟩)
    apply parent (.grantAs creator ⟨"enrollment_records", id, "reader", .actor recipient⟩)
  else state

theorem failed_network_reader_grant_aborts_publication (state : State)
    (creator recipient : DID) (id node : String) (created parent committed : Bool) :
    publishNetworkRecord state creator recipient id node created parent false committed = state := by
  simp [publishNetworkRecord]

theorem enrollment_grant_does_not_authorize_application_documents :
    let state : State := { objects := [⟨"nodes", "home", "user"⟩,
      ⟨"children", "application", "runtime"⟩], relations := [], enrolled := ["candidate"] }
    let published := publishNetworkRecord state "runtime" "candidate" "decision" "home"
      true true true true
    (allowed 5 published "candidate" "enrollment_records" "decision" "read",
     allowed 5 published "candidate" "children" "application" "read") = (true, false) := by
  native_decide

def localRegistryWriterFixture : State :=
  { objects := [⟨"nodes", "local-node", "user"⟩, ⟨"discovery", "self", "runtime"⟩],
    relations := [⟨"discovery", "self", "local_publication", .actor "runtime"⟩,
      ⟨"discovery", "self", "parent", .object "nodes" "local-node"⟩,
      ⟨"discovery", "self", "writer", .actor "user"⟩], enrolled := [] }

def localRegistryWriterRevoked : State :=
  apply localRegistryWriterFixture (.revokeAs "runtime"
    ⟨"discovery", "self", "writer", .actor "user"⟩)

theorem local_registry_writer_revocation_is_not_republished :
    let after := replicaGrant localRegistryWriterRevoked "discovery" "self" "runtime"
      "local-node" "user" true true true true true
    (allowed 5 localRegistryWriterFixture "user" "discovery" "self" "update",
     allowed 5 after "user" "discovery" "self" "update") = (true, false) := by native_decide

/-- A user-created secret configuration and both native relationships share
the transaction. Failure to resolve its Node or stage either grant aborts the
creation; a later update does not reinstall relationships. -/
def publishSecret (state : State) (user runtime : DID) (id node : String)
    (created parentGranted writerGranted committed : Bool) : State :=
  if created && parentGranted && writerGranted && committed then
    let created := directCreate state user "secret_configuration" id true true
    let parent := apply created
      (.grantAs user ⟨"secret_configuration", id, "parent", .object "nodes" node⟩)
    apply parent (.grantAs user ⟨"secret_configuration", id, "runtime_writer", .actor runtime⟩)
  else state

theorem failed_secret_parent_aborts_creation (state : State) (user runtime : DID)
    (id node : String) (created writer committed : Bool) :
    publishSecret state user runtime id node created false writer committed = state := by
  simp [publishSecret]

theorem operation_publication_is_visible_to_node_owner :
    let initial : State := { objects := [⟨"nodes", "home", "user"⟩], relations := [], enrolled := [] }
    let published := publishOperation initial "runtime" "status" "home" true true true
    (allowed 5 published "runtime" "operations" "status" "update",
     allowed 5 published "user" "operations" "status" "read",
     allowed 5 published "user" "operations" "status" "update",
     allowed 5 published "agent" "operations" "status" "read",
     allowed 5 published "other" "operations" "status" "read") =
      (true, true, false, false, false) := by native_decide

/-- Revoking a secret's runtime writer stops updates, but a separately retained
Node runtime-reader relation still grants read through the parent. -/
theorem node_runtime_reader_survives_secret_writer_revocation :
    let state : State :=
      { objects := [⟨"nodes", "node", "user"⟩, ⟨"secret_configuration", "secret", "user"⟩],
        relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
          ⟨"secret_configuration", "secret", "parent", .object "nodes" "node"⟩], enrolled := [] }
    (allowed 5 state "runtime" "secret_configuration" "secret" "read",
     allowed 5 state "runtime" "secret_configuration" "secret" "update") = (true, false) := by
  native_decide

/-- Some node-local enrollment records precede the Node document. They are
runtime-owned and private until a later transaction attaches the live Node
and records completion. Relationship creation and the marker share a native
transaction; a completed marker is not a claim that a grant was not revoked. -/
structure DeferredOperation where
  parent : Option String
  parentGranted : Bool
  parentLinked : Bool
  deriving Repr, DecidableEq

def beginDeferredOperation : DeferredOperation := ⟨none, false, false⟩

def attachDeferredOperation (state : DeferredOperation)
    (runtime embeddedRuntime : DID) (nativeOwner : Option DID)
    (node : Option String) (nodeRegistered committed : Bool) : DeferredOperation :=
  if !state.parentLinked && !runtime.isEmpty && runtime == embeddedRuntime &&
      nativeOwner == some runtime && node.any (fun id => !id.isEmpty) && nodeRegistered && committed then
    ⟨node, true, true⟩
  else state

theorem deferred_operation_stays_private_before_node :
    attachDeferredOperation beginDeferredOperation "runtime" "runtime" (some "runtime")
      none false true = beginDeferredOperation := by decide

theorem deferred_operation_failed_commit_is_unchanged :
    attachDeferredOperation beginDeferredOperation "runtime" "runtime" (some "runtime")
      (some "node") true false = beginDeferredOperation := by decide

theorem deferred_operation_foreign_owner_cannot_attach :
    attachDeferredOperation beginDeferredOperation "runtime" "runtime" (some "other")
      (some "node") true true = beginDeferredOperation := by decide

theorem completed_deferred_operation_cannot_restore_revocation :
    let published := attachDeferredOperation beginDeferredOperation "runtime" "runtime"
      (some "runtime") (some "node") true true
    let revoked := { published with parentGranted := false }
    attachDeferredOperation revoked "runtime" "runtime" (some "runtime") (some "node") true true = revoked := by
  decide

/-- Applied pairing observations are maintained by the runtime, including
before a user-owned Node exists. Their deferred parent publication uses the
same atomic marker as other bootstrap operations; a refresh of a completed
row cannot restore an owner-revoked relationship. Pairing data witnesses the
applied route and does not itself grant enrollment authority. -/
def refreshPairingPublication (state : DeferredOperation)
    (runtime embeddedRuntime : DID) (nativeOwner : Option DID)
    (node : Option String) (nodeRegistered committed : Bool) : DeferredOperation :=
  attachDeferredOperation state runtime embeddedRuntime nativeOwner node nodeRegistered committed

theorem applied_pairing_before_node_remains_private :
    refreshPairingPublication beginDeferredOperation "runtime" "runtime" (some "runtime")
      none false true = beginDeferredOperation := by decide

theorem applied_pairing_refresh_cannot_restore_revoked_parent :
    let revoked : DeferredOperation := ⟨some "node", false, true⟩
    refreshPairingPublication revoked "runtime" "runtime" (some "runtime")
      (some "node") true true = revoked := by decide

/-- Replicated observations from different runtimes may name the same peer.
Each runtime reconciles only its own source partition; peer identity alone
cannot authorize deletion or overwrite of a foreign observation. -/
def pairingInPartition (runtime source nativeOwner peer expectedPeer : String) : Bool :=
  runtime == source && runtime == nativeOwner && peer == expectedPeer

theorem foreign_pairing_is_not_a_local_twin (runtime foreign owner peer : String)
    (different : runtime ≠ foreign) :
    pairingInPartition runtime foreign owner peer peer = false := by
  simp [pairingInPartition, different]

theorem forged_pairing_source_is_not_a_local_twin (runtime foreign peer : String)
    (different : runtime ≠ foreign) :
    pairingInPartition runtime runtime foreign peer peer = false := by
  simp [pairingInPartition, different]

/-- Enrollment route materialization is authored by the local endpoint's
held identity. On a desktop client the desired row names the remote owner Node,
which is not the local author and need not be stored on the client. The local
author owns the row; its optional local Node parent follows the same deferred
publication contract as other local operational records. -/
def desiredRoutePublisher (actor localNode targetNode : DID) (held : Bool) : Bool :=
  held && !actor.isEmpty && actor == localNode && !targetNode.isEmpty

theorem desktop_can_publish_for_remote_node :
    desiredRoutePublisher "client" "client" "remote" true = true := by decide

theorem desired_route_rejects_foreign_or_unheld_author :
    [desiredRoutePublisher "other" "client" "remote" true,
     desiredRoutePublisher "client" "client" "remote" false] = [false, false] := by decide

/-- A route witness can inform admission only when its native owner is the
runtime for the requested Node. Writable DID and peer fields are not proof of
that authority. Both desired and applied witnesses are required. -/
def runtimeRouteWitnesses (runtime desiredOwner appliedOwner : DID)
    (desiredVisible appliedVisible : Bool) : Bool :=
  desiredVisible && appliedVisible && runtime == desiredOwner && runtime == appliedOwner

theorem forged_route_witness_does_not_authorize :
    [runtimeRouteWitnesses "node" "other" "node" true true,
     runtimeRouteWitnesses "node" "node" "other" true true,
     runtimeRouteWitnesses "node" "node" "node" true true] =
    [false, false, true] := by decide

/-- A native relationship on one Local ACP store does not itself establish
the same relationship on another store. Exact document delivery supplies no
such premise; the receiver's native relation must be separately established by
an authenticated owner-authorized mechanism. -/
def receiverHasGrant (senderGranted relationshipDelivered : Bool) : Bool :=
  senderGranted && relationshipDelivered

theorem sender_grant_without_relationship_delivery_is_not_receiver_grant :
    receiverHasGrant true false = false := by decide

/-- The marker is publication progress for one exact pairing document and
one exact Agent, not an authorization decision. The native relation is the
read authority. A new entitlement generation must be explicitly authorized
by the configuration owner and committed with the relation. Disabling the
entitlement removes the relation; a revoked relation is not silently restored
by observing the same generation again. -/
structure PairingReaderPublication where
  lastGrantedGeneration : Option Nat
  readerGranted : Bool
  deriving Repr, DecidableEq

def reconcilePairingReader (state : PairingReaderPublication)
    (entitled ownerAuthorized committed : Bool) (generation : Nat) : PairingReaderPublication :=
  if !committed then state
  else if !entitled then { state with readerGranted := false }
  else if ownerAuthorized && state.lastGrantedGeneration != some generation then
    ⟨some generation, true⟩
  else state

theorem pairing_reader_denial_never_grants (state : PairingReaderPublication)
    (generation : Nat) :
    (reconcilePairingReader state false true true generation).readerGranted = false := by
  simp [reconcilePairingReader]

theorem pairing_reader_revocation_is_not_refreshed (generation : Nat) :
    reconcilePairingReader ⟨some generation, false⟩ true true true generation =
      ⟨some generation, false⟩ := by simp [reconcilePairingReader]

theorem pairing_reader_new_generation_requires_owner_authorization (generation : Nat) :
    reconcilePairingReader ⟨none, false⟩ true false true generation = ⟨none, false⟩ := by
  simp [reconcilePairingReader]

theorem failed_pairing_reader_commit_is_unchanged (state : PairingReaderPublication)
    (entitled ownerAuthorized : Bool) (generation : Nat) :
    reconcilePairingReader state entitled ownerAuthorized false generation = state := by
  simp [reconcilePairingReader]

/-- A document owner can attach a Node parent without proving that it is the
Node named by the document. Native ownership and a parent tuple alone therefore
do not authenticate a readiness publisher or its target. -/
def forgedParent : State :=
  apply (directCreate
    { objects := [⟨"nodes", "home", "user"⟩], relations := [], enrolled := [] }
    "other" "operations" "forged" true true)
    (.grantAs "other" ⟨"operations", "forged", "parent", .object "nodes" "home"⟩)

theorem forged_operation_can_inherit_parent_read :
    allowed 5 forgedParent "user" "operations" "forged" "read" = true := by
  native_decide

/-- Installing a policy on an existing unbound collection can leave legacy
objects readable without registered ownership. Only an absent collection or
one already bound to the expected policy is eligible for this fresh path. A
matching schema does not itself verify registration of its existing objects. -/
inductive CollectionBinding where
  | absent | expected | unbound | different
  deriving BEq, Repr

def readinessPreflight : CollectionBinding → Bool
  | .absent | .expected => true
  | .unbound | .different => false

theorem unbound_readiness_rejected : readinessPreflight .unbound = false := by rfl

/-- Recovery authenticates the runtime against native ownership and compares
the stored physical parent with the uniquely observed current Node. These are
fresh observations, not a native transaction fence on the later grant. -/
def readinessRecoveryEligible (pending : Bool) (runtime embeddedRuntime : DID) (parentRegistered : Bool)
    (nativeOwner recordedParent currentParent : Option String) : Bool :=
  pending && !runtime.isEmpty && runtime == embeddedRuntime && parentRegistered && nativeOwner == some runtime &&
    recordedParent.isSome && recordedParent == currentParent

def repairReadiness (state : ParentPublication) (runtime embeddedRuntime : DID) (parentRegistered : Bool)
    (nativeOwner recordedParent currentParent : Option String)
    (grantSucceeded markSucceeded : Bool) : ParentPublication :=
  if readinessRecoveryEligible (!state.parentLinked) runtime embeddedRuntime parentRegistered nativeOwner recordedParent currentParent then
    repairParent state grantSucceeded markSucceeded
  else state

theorem readiness_unknown_owner_cannot_repair (state : ParentPublication) (runtime embeddedRuntime : DID)
    (parentRegistered : Bool)
    (recordedParent currentParent : Option String) (grantSucceeded markSucceeded : Bool) :
    repairReadiness state runtime embeddedRuntime parentRegistered none recordedParent currentParent grantSucceeded markSucceeded = state := by
  simp [repairReadiness, readinessRecoveryEligible]

theorem readiness_wrong_parent_cannot_repair (state : ParentPublication) (runtime embeddedRuntime owner : DID)
    (parentRegistered : Bool)
    (recordedParent currentParent : Option String) (different : recordedParent ≠ currentParent)
    (grantSucceeded markSucceeded : Bool) :
    repairReadiness state runtime embeddedRuntime parentRegistered (some owner) recordedParent currentParent grantSucceeded markSucceeded = state := by
  simp [repairReadiness, readinessRecoveryEligible, different]

theorem readiness_foreign_runtime_cannot_repair (state : ParentPublication) (runtime embeddedRuntime : DID)
    (different : runtime ≠ embeddedRuntime) (parentRegistered : Bool)
    (nativeOwner recordedParent currentParent : Option String) (grantSucceeded markSucceeded : Bool) :
    repairReadiness state runtime embeddedRuntime parentRegistered nativeOwner recordedParent currentParent
      grantSucceeded markSucceeded = state := by
  simp [repairReadiness, readinessRecoveryEligible, different]

theorem readiness_unregistered_parent_cannot_repair (state : ParentPublication) (runtime embeddedRuntime : DID)
    (nativeOwner recordedParent currentParent : Option String) (grantSucceeded markSucceeded : Bool) :
    repairReadiness state runtime embeddedRuntime false nativeOwner recordedParent currentParent
      grantSucceeded markSucceeded = state := by
  simp [repairReadiness, readinessRecoveryEligible]

theorem failed_readiness_grant_cannot_mark_complete :
    (repairReadiness beginParentPublication "runtime" "runtime" true (some "runtime")
      (some "node") (some "node") false true).parentLinked = false := by decide

theorem readiness_grant_survives_failed_marker :
    let first := repairReadiness beginParentPublication "runtime" "runtime" true (some "runtime")
      (some "node") (some "node") true false
    let second := repairReadiness first "runtime" "runtime" true (some "runtime")
      (some "node") (some "node") false true
    first.parentGranted = true ∧ first.parentLinked = false ∧ second.parentLinked = true := by decide

/-- A stale worker may have observed an incomplete marker before another
worker completed publication and an owner revoked the grant. A detached ACP1
grant can then restore the relationship; a pending marker is not a fence. -/
theorem stale_readiness_repair_can_restore_revocation :
    let linked := repairReadiness beginParentPublication "runtime" "runtime" true (some "runtime")
      (some "node") (some "node") true true
    let revoked := ownerRevokesLinkedParent linked
    (finishStaleParentGrant revoked true true).parentGranted = true := by decide

/-- A reader authenticates a readiness row using its native owner and the
physical live Node, not the row's writable node DID alone. Counts and owners
are observations from the same read transaction. They do not fence later
deletion, replacement, permission revocation, or a downstream write. -/
structure ReadinessObservation where
  requestedRuntime : DID
  embeddedRuntime : DID
  nodeCount : Nat
  readinessCount : Nat
  nodeId : Option String
  nodeOwner : Option DID
  readinessOwner : Option DID
  recordedParent : Option String
  parentLinked : Bool
  parentPresent : Bool
  deriving Repr

def readinessObservationUsable (o : ReadinessObservation) : Bool :=
  !o.requestedRuntime.isEmpty && o.requestedRuntime == o.embeddedRuntime &&
    o.nodeCount == 1 && o.readinessCount == 1 &&
    o.nodeId.any (fun id => !id.isEmpty) && o.nodeOwner.any (fun owner => !owner.isEmpty) &&
    o.readinessOwner == some o.requestedRuntime && o.recordedParent == o.nodeId && o.parentLinked

/-- The completion marker records publication progress; it cannot establish
that an owner has not subsequently revoked the native relationship. -/
theorem readiness_marker_does_not_prove_current_relation :
    let o : ReadinessObservation := ⟨"runtime", "runtime", 1, 1, some "node", some "user",
      some "runtime", some "node", true, false⟩
    readinessObservationUsable o = true ∧ o.parentPresent = false := by decide

theorem readiness_unregistered_owner_is_unusable (o : ReadinessObservation)
    (missing : o.readinessOwner = none) : readinessObservationUsable o = false := by
  simp [readinessObservationUsable, missing]

theorem readiness_pending_is_unusable (o : ReadinessObservation)
    (pending : o.parentLinked = false) : readinessObservationUsable o = false := by
  simp [readinessObservationUsable, pending]

/-- Goal reconciliation uses the live runtime status owner. Persisted
NodeRuntime is observable data and cannot establish that the runtime settled. -/
structure LiveReconcileStatus where
  idle : Bool
  lastResultKnown : Bool
  lastResultError : Bool
  deriving Repr

def goalRuntimeSettled (status : Option LiveReconcileStatus) (_persistedSettled : Bool) : Bool :=
  match status with
  | none => false
  | some live => live.idle && live.lastResultKnown && !live.lastResultError

theorem missing_live_runtime_is_not_settled (persisted : Bool) :
    goalRuntimeSettled none persisted = false := by rfl

theorem persisted_runtime_cannot_change_settled (status : Option LiveReconcileStatus) :
    goalRuntimeSettled status false = goalRuntimeSettled status true := by
  cases status <;> rfl

end Identity.Operations
