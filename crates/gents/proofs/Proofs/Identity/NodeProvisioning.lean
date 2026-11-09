import Proofs.Identity.State
import Proofs.RuntimeReconcile.HttpAccess

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

/-- Runtime invariant: the runtime operates only with node ACP (NAC) enabled.
Every authorization decision in this model assumes it. Native premise: while
NAC is not `Enabled`, DefraDB bypasses node-permission checks and reports every
actor as admin (`NodeACP::is_admin`); disabling NAC is an admin-class native
operation, so an admin can turn it off at runtime, and native persists
`DisabledTemporarily` across restarts (only re-enable turns it back on). Every
node open enables NAC, but that does not exclude a store reopened disabled. A
node observed without NAC enabled is therefore refused before the runtime
admits work (fail closed), never treated as "everyone is admin"; the fix is to
re-enable NAC as its owner or an admin. Destructive admin decisions refuse it
again as defence in depth (`Configuration.destructiveAdminDecision`). -/
def runtimeMayStart (status : NacStatus) : Bool := status == .enabled

theorem runtime_refuses_without_enabled_nac (status : NacStatus)
    (notEnabled : status ≠ .enabled) : runtimeMayStart status = false := by
  cases status <;> simp_all [runtimeMayStart]

theorem disabled_nac_is_refused_at_startup :
    runtimeMayStart .disabledTemporarily = false ∧ runtimeMayStart .notConfigured = false ∧
      runtimeMayStart .enabled = true := by decide

/-! Local home ownership and local startup (#3609, #3611). -/

/-- A provisioned local home as its `init.json` records it: the home's Node
DID and the DID `init` granted HouseholdOwner (`owner_did`). Homes written
before one User key per machine carry no owner. -/
structure HomeRecord where
  node : DID
  owner : Option DID
  deriving DecidableEq, Repr

/-- The owner a fresh bootstrap records: the machine's User DID, exactly when
the bootstrap may grant (`bootstrapMayGrant`); a refused bootstrap records no
owner. -/
def provisionedOwner (user : DID) (o : BootstrapObservation) : Option DID :=
  if bootstrapMayGrant o then some user else none

inductive LocalHomeAdmission where
  | serve
  | refuseFreshStart
  deriving DecidableEq, Repr

/-- A machine has one User key (`<desktop root>/principal.ed25519.key`),
shared by the desktop, the CLI and the shims; the desktop's embedded node
already signs with it and remote enrollment presents its DID. `init` grants
that DID HouseholdOwner, so the local home's admin is the User itself and the
desktop reads the home over HTTP as its admin without enrolling into its own
node. Decision (user, #3609): a separate client key that the desktop
self-paired as admin is removed, and the client reaches its local home only
when the home records this User as owner.

A home without an owner (written under the two-key scheme) or with another
owner is refused with a fresh-start remedy, never converted: granting the
User admin on such a home would need an authority the client does not hold,
and re-granting through the old self-pairing path is exactly what this removes
(a failed grant could roll back the home's only admin). -/
def admitLocalHome (user : DID) (h : HomeRecord) : LocalHomeAdmission :=
  if h.owner = some user then .serve else .refuseFreshStart

theorem admitted_iff_user_owns (user : DID) (h : HomeRecord) :
    admitLocalHome user h = .serve ↔ h.owner = some user := by
  unfold admitLocalHome
  split <;> simp_all

/-- A home provisioned before one User key per machine records no owner. -/
theorem legacy_home_refused (user node : DID) :
    admitLocalHome user ⟨node, none⟩ = .refuseFreshStart := by
  simp [admitLocalHome]

/-- A home owned by any other DID is refused. This includes every home the
previous User key provisioned after a desktop full reset retired that key and
a new one was created: such homes stay refused until started fresh. -/
theorem foreign_owner_refused (user other node : DID) (foreign : other ≠ user) :
    admitLocalHome user ⟨node, some other⟩ = .refuseFreshStart := by
  simp [admitLocalHome, foreign]

theorem provisioned_home_admitted (user node : DID) (o : BootstrapObservation)
    (granted : bootstrapMayGrant o = true) :
    admitLocalHome user ⟨node, provisionedOwner user o⟩ = .serve := by
  simp [admitLocalHome, provisionedOwner, granted]

theorem refused_bootstrap_home_refused (user node : DID) (o : BootstrapObservation)
    (refused : bootstrapMayGrant o = false) :
    admitLocalHome user ⟨node, provisionedOwner user o⟩ = .refuseFreshStart := by
  simp [admitLocalHome, provisionedOwner, refused]

/-- A key created after a full reset does not inherit the homes its
predecessor provisioned (`foreign_owner_refused`). -/
theorem regenerated_key_refuses_prior_home (previous current node : DID)
    (o : BootstrapObservation) (regenerated : previous ≠ current) :
    admitLocalHome current ⟨node, provisionedOwner previous o⟩ = .refuseFreshStart := by
  unfold provisionedOwner
  split
  · exact foreign_owner_refused current previous node regenerated
  · exact legacy_home_refused current node

/-- Steps a native recorder reports while the client brings its local home
up. `enroll` and `approve` name the removed self-pairing steps so a recorder
can report them; local startup never produces them. -/
inductive LocalStartupStep where
  | provision
  | launch
  | statusRead (signer : DID)
  | recordLocal
  | clientStart
  | enroll
  | approve
  deriving DecidableEq, Repr

/-- Provision (a no-op on an initialized home), launch, read the private
status as the User, record the local node over HTTP only, then start the
client. The status read is local readiness. -/
def localStartup (user : DID) : List LocalStartupStep :=
  [.provision, .launch, .statusRead user, .recordLocal, .clientStart]

/-- No enrollment or approval occurs during local startup, so none precedes
local readiness. -/
theorem local_startup_has_no_enrollment (user : DID) (step : LocalStartupStep)
    (h : step ∈ localStartup user) : step ≠ .enroll ∧ step ≠ .approve := by
  simp [localStartup] at h
  rcases h with h | h | h | h | h <;> subst h <;> simp

theorem local_status_read_signed_by_user (user signer : DID)
    (h : LocalStartupStep.statusRead signer ∈ localStartup user) : signer = user := by
  simp [localStartup] at h
  exact h

/-- Local readiness (the User's private status read) precedes recording the
local node and starting the client. -/
theorem local_readiness_precedes_client (user : DID) :
    (localStartup user).take 3 = [.provision, .launch, .statusRead user] := by
  rfl

/-- The provisioned owner is classified `admin` by native NAC, and the private
`/status` read is dispatched as that admin, never as the served node. -/
theorem provisioned_user_reads_private_status (user node : DID) (o : BootstrapObservation)
    (granted : bootstrapMayGrant o = true) :
    admitLocalHome user ⟨node, provisionedOwner user o⟩ = .serve ∧
      RuntimeReconcile.HttpAccess.decide true .bootstrap .admin = .operator ∧
      RuntimeReconcile.HttpAccess.readsAs true .bootstrap .admin = some .admin := by
  refine ⟨provisioned_home_admitted user node o granted, rfl, rfl⟩

end Identity.NodeProvisioning
