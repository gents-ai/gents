import Proofs.Identity.State

namespace Identity.NodeProvisioning

/-- These node permissions are separate from document ACP. Their presence does
not establish document authorization, and unbound collections do not acquire
document protection from them. Extra operations require their own justification. -/
def requiredPermissions : List String := ["read-document", "update-document", "add-dac-relation"]

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

theorem native_denial_cannot_publish (o : AgentCreatorObservation)
    (denied : o.nativeCreate = false) : agentCreatorMayPublish o = false := by
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
default Agent; configuration is published after them. A lost or ambiguous
reservation response grants nothing; after a crash, a durable reservation cannot authorize replay. -/
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

end Identity.NodeProvisioning
