import Proofs.Store.Identity.State

namespace Identity.NodeProvisioning

/-- These node permissions are separate from document ACP. Their presence does
not establish document authorization, and unbound collections do not acquire
document protection from them. Extra operations require their own justification.

The native collection selector and relationship publication paths require
`get-collection` even when an Agent already holds document read/update and
DAC relation permissions. Goal control exercises these paths as the executing
Agent. This read-only metadata permission enables those operations without
schema mutation, document access, or a different transaction identity.

`delete-dac-relation` is the revoke of `add-dac-relation` (maintainer decision,
2026-11-12, gents#3683): whoever can grant access on a document can revoke it,
so an Agent that links or shares a document it manages can withdraw that
relationship as itself. Like the add, it is only the native operation gate;
document ACP still admits a change only from the document's owner or a
declared manager of that relation (`Household.mayManage`), and a removed tuple
only narrows access (`Household.revocation_only_narrows`).

`get-collection` is here because every document write needs it (maintainer
decision, 2026-11-13, gents#3747). External premise (defradb.rs #2744,
`NodePermission::go_order`): the pinned DefraDB checks `get-collection` before
`update-document` and `delete-document`, inside and outside a transaction, so
an Agent without it can write none of its requests, outputs or tool calls. The
permission reads a collection's definition, never its documents, and changes
no schema (`GrantSets.schemaChangePermissions` excludes it). -/
def requiredPermissions : List String :=
  ["get-collection", "read-document", "update-document", "add-dac-relation",
   "delete-dac-relation"]

/-- An executing Agent carries its own signer. Native NAC decides whether that
actor may create documents and manage the new document relationships; the
runtime alone derives keys. The identity may be allocated before native
creation; its node capabilities are projected from the published Agent
document as the publishing actor (`GrantSets.projectBaseline`), never granted
by the runtime on that actor's behalf. -/
structure AgentCreatorObservation where
  runtimeMatches : Bool
  signerHeld : Bool
  signerMatches : Bool
  nativeCreate : Bool
  nativeRelation : Bool
  deriving Repr

def agentCreatorMayPublish (o : AgentCreatorObservation) : Bool :=
  o.runtimeMatches && o.signerHeld && o.signerMatches && o.nativeCreate && o.nativeRelation

theorem missing_signer_cannot_publish (o : AgentCreatorObservation)
    (missing : o.signerHeld = false) : agentCreatorMayPublish o = false := by
  simp [agentCreatorMayPublish, missing]

/-- External premise: an enabled Agent's native document create and relation
permissions are projected from its live Agent document as the Agent baseline,
so production never observes them denied to an authorized creator; a denial
still refuses the publication. -/
theorem native_denial_cannot_publish (o : AgentCreatorObservation)
    (denied : o.nativeCreate = false) : agentCreatorMayPublish o = false := by
  simp [agentCreatorMayPublish, denied]

theorem native_relation_denial_cannot_publish (o : AgentCreatorObservation)
    (denied : o.nativeRelation = false) : agentCreatorMayPublish o = false := by
  simp [agentCreatorMayPublish, denied]

/-- The Agent command is exposed by either configured category. This is tool
availability only; Agent reads, updates, creates and relationship grants remain
subject to native authorization as the executing Agent. -/
def agentCommandExposed (nodeCategory agentCategory : Bool) : Bool :=
  nodeCategory || agentCategory

theorem agent_category_exposes_commands (nodeCategory : Bool) :
    agentCommandExposed nodeCategory true = true := by
  cases nodeCategory <;> rfl

/-- Before any bootstrap NAC grant, exclusive local initialization durably
consumes a one-shot marker for a verified fresh store. The authenticated client
and matching runtime principal are separately required. An absent Node means an
authoritative absence, not an empty actor-filtered read. The bootstrap grants
are the direct sets `GrantSets.directSets` names for the init User and the
default Agent, issued as the NAC owner (`nacOwner`); configuration is published
after them. A lost or ambiguous reservation response grants nothing; after a
crash, a durable reservation cannot authorize replay. -/
structure BootstrapObservation where
  exclusive : Bool
  matchingRuntime : Bool
  authenticatedClient : Bool
  nodeAbsentObserved : Bool
  storeEmptyObserved : Bool
  reservationDurable : Bool
  nacEnabled : Bool
  freshEvidence : Bool
  configurationPublished : Bool
  deriving Repr

def reservationConfirmed (created fileSynced directorySynced : Bool) : Bool :=
  created && fileSynced && directorySynced

/-- Identity preparation precedes consuming the fresh home's one-shot marker.
Once consumed, the marker still cannot be replayed after a later failure. -/
def reservationAfterIdentity (alreadyReserved identityReady : Bool) : Bool :=
  alreadyReserved || identityReady

theorem identity_failure_keeps_fresh_reservation_available :
    reservationAfterIdentity false false = false := by rfl

theorem identity_failure_never_clears_a_consumed_reservation :
    reservationAfterIdentity true false = true := by rfl

theorem ambiguous_reservation_cannot_authorize (created fileSynced : Bool) :
    reservationConfirmed created fileSynced false = false := by
  cases created <;> cases fileSynced <;> rfl

def bootstrapMayGrant (o : BootstrapObservation) : Bool :=
  o.exclusive && o.matchingRuntime && o.authenticatedClient && o.nodeAbsentObserved &&
    o.storeEmptyObserved && o.reservationDurable && o.nacEnabled && o.freshEvidence &&
    !o.configurationPublished

def bootstrapAfterCrash (o : BootstrapObservation) : BootstrapObservation :=
  { o with freshEvidence := false }

theorem bootstrap_crash_cannot_replay (o : BootstrapObservation) :
    bootstrapMayGrant (bootstrapAfterCrash o) = false := by
  simp [bootstrapMayGrant, bootstrapAfterCrash]

theorem bootstrap_does_not_regrant_existing (o : BootstrapObservation)
    (existing : o.nodeAbsentObserved = false) : bootstrapMayGrant o = false := by
  simp [bootstrapMayGrant, existing]

theorem bootstrap_needs_durable_reservation (o : BootstrapObservation)
    (unreserved : o.reservationDurable = false) : bootstrapMayGrant o = false := by
  simp [bootstrapMayGrant, unreserved]

/-- Starting a server does not grant the Node owner new NAC permissions.
Missing or revoked permissions on an existing identity require an explicit
authorized operation, not implicit startup adoption. -/
def nodeOwnerGrantsAfterStartup (before : List String) : List String := before

theorem startup_preserves_node_owner_revocation (before : List String) (permission : String) :
    nodeOwnerGrantsAfterStartup (before.filter (· != permission)) =
      before.filter (· != permission) := by rfl

/-- Native NAC status (`acp::nac::NacStatus`). -/
inductive NacStatus where
  | notConfigured
  | enabled
  | disabledTemporarily
  deriving DecidableEq, Repr

def NacStatus.name : NacStatus → String
  | .notConfigured => "not_configured"
  | .enabled => "enabled"
  | .disabledTemporarily => "disabled_temporarily"

/-- Who owns node access control (NAC) on a home's store.

External premise (the pinned DefraDB revision, defradb.rs #2744): while NAC is
enabled, the NAC owner and every holder of `bypass-dac` (the native `admin`
relation confers it) read past document ACP. The node's own DID passes every
node-operation check whatever NAC holds for it (`check_node_access`), but a NAC
relationship change has no such exception: native admits it only from the
owner, an admin, or a delegate holding both the operation and the permission.

Decision (2026-11-17): the init User owns NAC, named when its fresh store is
first opened; the Node never owns it and holds only the permissions the owner
grants it then (`GrantSets.nodeRuntimePermissions`: all but `bypass-dac` and
the enforcement switches), so the runtime reads only what document ACP grants
the actor it runs as.
Every NAC relationship change the runtime principal makes in process (the
bootstrap, trial and enrollment grants among them) is issued as that owner:
native takes an in-process call's identity as named, so the runtime names the
owner without holding its key and keeps the NAC authority it had as owner,
without its read bypass. Over HTTP the owning User issues them with its own
client. -/
inductive NacOwner where
  | initUser
  | node
  deriving DecidableEq, Repr

def nacOwner : NacOwner := .initUser

/-- Runtime invariant: the runtime operates only with node ACP (NAC) enabled,
and only on a node whose own identity does not read past document ACP.
Every authorization decision in this model assumes both. Native premise: while
NAC is not `Enabled`, DefraDB bypasses node-permission checks and reports every
actor as admin (`NodeACP::is_admin`); disabling NAC is an admin-class native
operation, so an admin can turn it off at runtime, and native persists
`DisabledTemporarily` across restarts (only re-enable turns it back on). Every
node open enables NAC, but that does not exclude a store reopened disabled. A
node observed without NAC enabled is therefore refused before the runtime
admits work (fail closed), never treated as "everyone is admin"; the fix is to
re-enable NAC as its owner or an admin. Destructive admin decisions refuse it
again as defence in depth (`Configuration.destructiveAdminDecision`).

`nodeReadsPastDac` is native NAC's answer for the Node's own DID and
`bypass-dac`: true for a store whose Node owns NAC (every store created before
`nacOwner` was the init User) and for a Node an admin made admin or granted
`bypass-dac`. Such a node is refused the same way: its Node would read every
actor's documents. A store whose Node owns NAC cannot be given another owner,
so it is not converted; it is refused with the instruction to set the home up
again. -/
def runtimeMayStart (status : NacStatus) (nodeReadsPastDac : Bool) : Bool :=
  status == .enabled && !nodeReadsPastDac

/-- External premise: `FreshBackendStore::initialize` configures NAC on every
store and native persists that configuration, so a production start observes
NAC enabled or disabled and never unconfigured; any status but enabled refuses. -/
theorem runtime_refuses_without_enabled_nac (status : NacStatus) (nodeReadsPastDac : Bool)
    (notEnabled : status ≠ .enabled) : runtimeMayStart status nodeReadsPastDac = false := by
  cases status <;> simp_all [runtimeMayStart]

theorem runtime_refuses_a_node_reading_past_dac (status : NacStatus) :
    runtimeMayStart status true = false := by
  simp [runtimeMayStart]

theorem disabled_nac_is_refused_at_startup :
    runtimeMayStart .disabledTemporarily false = false ∧
      runtimeMayStart .notConfigured false = false ∧
      runtimeMayStart .enabled false = true := by decide

/-- A store set up with `nacOwner` starts: its Node owns nothing and reads
past no document ACP until an admin grants it that. -/
theorem init_user_owned_store_may_start :
    runtimeMayStart .enabled (nacOwner == .node) = true := by decide

end Identity.NodeProvisioning
