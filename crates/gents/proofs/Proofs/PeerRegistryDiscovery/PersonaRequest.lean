import Proofs.Configuration
import Proofs.Basic
import Proofs.ToolPolicy.Meet
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Prod

/-!
# Authenticated persona commands

Persona requests are command DTOs, not installed configuration. Published composer
choices validate the request; they do not prove the resulting references resolve.
The shared authoring loader compiles the requested edits into canonical documents.
This boundary admits the command and resolves its candidate behavior through
`Configuration.resolveBehavior`. Publication/idempotence belong to ApplyReconcile,
not a second behavior/tools materializer here. Exact translation of composer
root/preset/profile choices and copying a clone payload are loader refinement
obligations; resolving the candidate does not itself prove those translations.
The composer selects inference through `profile` only — there is no separate
backend/model field, catalog, or fallback in the request shape.
Catalog construction must filter profiles and allowed roots for the authorized
principal; the old Rust loader’s unfiltered catalogs do not meet this premise.
-/

namespace PeerRegistryDiscovery
namespace PersonaRequest

/-- The exact current enrollment generation supplied by the single durable
authority owner. Persona requests never infer authority from route presence. -/
structure EnrollmentAuthorization where
  network : String
  memberDid : String
  memberPeer : String
  ownerAgent : String
  requestDigest : String
  sequence : Nat
  authorizationExpiresAt : String
  deriving DecidableEq, Repr

inductive AuthorityKind
  | enrollment
  | localSelf
  deriving DecidableEq, Repr

/-- The requested operation. `clone_from` is folded into `create` in the
Rust code; here the request carries `cloneFrom` as a field and `create`
branches on whether it is empty (mirroring `PersonaOp::Create { clone_from }`). -/
inductive Op
  | create
  | edit
  | disable
  deriving DecidableEq, Repr

/-- Edit payloads are patches, not replacement documents. `omitted` retains
the stored value, `clear` removes an optional value, and `set` replaces it.
Create continues to use the required composer values on `Request`; this type
models the edit field-presence mask carried by the signed command DTO. -/
inductive FieldUpdate where
  | omitted
  | clear
  | set (value : String)
  deriving DecidableEq, Repr

def FieldUpdate.apply (update : FieldUpdate) (stored : Option String) : Option String :=
  match update with
  | .omitted => stored
  | .clear => none
  | .set value => some value

theorem omitted_field_preserves (stored : Option String) :
    FieldUpdate.omitted.apply stored = stored := rfl

theorem explicit_clear_is_distinct (stored : String) :
    FieldUpdate.clear.apply (some stored) ≠ FieldUpdate.omitted.apply (some stored) := by
  simp [FieldUpdate.apply]

/-- The published options a request is validated against: the root and
inference-profile catalogs for the selected scope, plus its known (enabled)
principal DIDs — a request naming a phantom or foreign `agent_did` must be
rejected, never mint orphan config. -/
structure Catalog where
  roots : Finset String
  profiles : Finset String
  agents : Finset String
  authorization : Option EnrollmentAuthorization
  deriving DecidableEq

/-- A typed persona request. String payload fields carry the requested
values; `key` identifies the create command and its new behavior;
`agent` is the `agent_did` the request claims to configure. -/
structure Request where
  key : String
  op : Op
  agent : String
  requester : String
  network : String
  memberPeer : String
  enrollmentRequestDigest : String
  authorizationSequence : Nat
  authorizationExpiresAt : String
  authorityKind : AuthorityKind
  localSigner : String
  localSignatureValid : Bool
  name : String
  description : String
  systemPrompt : String
  root : String
  preset : String
  profile : String
  nameEdit : FieldUpdate
  descriptionEdit : FieldUpdate
  systemPromptEdit : FieldUpdate
  rootEdit : FieldUpdate
  presetEdit : FieldUpdate
  profileEdit : FieldUpdate
  makeDefault : Bool
  cloneFrom : String
  target : String
  deriving DecidableEq, Repr

/-- An explicit sibling-tools operation refines the canonical tool document inside
the existing self-config patch transaction, separately from signed creation.
Omission preserves current selections; explicit false revokes.
Selections never imply self-configuration or pack install.
Host execution and graph caller admission still use their existing owners. -/
def selectedToolFlag (requested : Option Bool) (existing : Bool) : Bool :=
  requested.getD existing

theorem omitted_tool_selection_preserves (existing : Bool) :
    selectedToolFlag none existing = existing := rfl

theorem explicit_tool_selection_wins (requested existing : Bool) :
    selectedToolFlag (some requested) existing = requested := rfl

/-- Network selection reuses the command-policy vocabulary and tool-policy
ordering. The focused configurator may preserve an existing selection or
explicitly narrow it to disabled; inherit/enabled are never admitted inputs. -/
def selectedNetworkMode
    (requested : Option CommandPolicy.NetworkMode)
    (existing : CommandPolicy.NetworkMode) : CommandPolicy.NetworkMode :=
  requested.getD existing

def networkSelectionAllowed : Option CommandPolicy.NetworkMode → Bool
  | none => true
  | some .disabled => true
  | some .inherit | some .enabled => false

theorem omitted_network_selection_preserves (existing : CommandPolicy.NetworkMode) :
    selectedNetworkMode none existing = existing := rfl

theorem only_disabled_network_selection_admitted
    (requested : CommandPolicy.NetworkMode) :
    networkSelectionAllowed (some requested) = true ↔ requested = .disabled := by
  cases requested <;> simp [networkSelectionAllowed]

theorem admitted_network_selection_does_not_widen
    (requested : Option CommandPolicy.NetworkMode)
    (existing : CommandPolicy.NetworkMode)
    (h : networkSelectionAllowed requested = true) :
    ToolPolicy.networkRank (selectedNetworkMode requested existing) ≤
      ToolPolicy.networkRank existing := by
  cases requested with
  | none => simp [selectedNetworkMode]
  | some requested =>
      cases requested <;> cases existing <;>
        simp [networkSelectionAllowed, selectedNetworkMode, ToolPolicy.networkRank] at h ⊢

/-- Observations are supplied by the existing identity-scoped config transaction.
The focused operation refuses protected or shared targets rather than mutating
other behaviors or inventing another materialization owner. -/
def siblingToolsAllowed (ownerMatches isProtected sharedContext sharedTools : Bool) : Bool :=
  ownerMatches && !isProtected && !sharedContext && !sharedTools

theorem protected_sibling_tools_denied (owner sharedContext sharedTools : Bool) :
    siblingToolsAllowed owner true sharedContext sharedTools = false := by
  cases owner <;> simp [siblingToolsAllowed]

theorem unshared_owned_sibling_tools_allowed :
    siblingToolsAllowed true false false false = true := rfl

/-- Native graph tool presentation is an independent opt-in. Presentation is
not graph caller admission and does not grant installation or configuration. -/
def graphToolPresented (requested : Bool) (_selfConfig _packInstall : Bool) : Bool :=
  requested

theorem graph_tools_without_configuration : graphToolPresented true false false = true := rfl
theorem configuration_does_not_grant_graph_tools :
    graphToolPresented false true true = false := rfl

/-- Read-only enabled/id projection for command target checks. Canonical references
and ownership are validated by resolution of the compiled candidate below. -/
structure BehaviorCatalog where
  behaviors : Finset (String × Bool)
  protectedIds : Finset String
  deriving DecidableEq

-- These admission conjuncts are `abbrev` (reducible) so the `Decidable`
-- instance for `admits` resolves through them via `infer_instance`.

/-- The two built-in preset names (`persona_presets::builtin_preset_names`). -/
abbrev presetKnown (r : Request) : Prop :=
  r.preset = "readonly" ∨ r.preset = "write"

abbrev nameOk (r : Request) : Prop := r.name ≠ ""

/-- A preset-based behavior is authored from scratch and must carry useful
operating instructions. A clone may inherit its source prompt when this field
is empty. -/
abbrev createPromptOk (r : Request) : Prop :=
  r.cloneFrom ≠ "" ∨ r.systemPrompt.trim ≠ ""

/-- An empty root selects the runtime cwd; a non-empty composer root must
be published. Existence and authority are checked by the host execution owner. -/
abbrev rootOk (cat : Catalog) (r : Request) : Prop :=
  r.root = "" ∨ r.root ∈ cat.roots

/-- `profile` is required for create, edit, and clone: no implicit model or
profile fallback exists in the request shape, so an empty/blank profile fails
admission even if such an id were present in the catalog. -/
abbrev profileOk (cat : Catalog) (r : Request) : Prop :=
  r.profile.trim ≠ "" ∧ r.profile ∈ cat.profiles

/-- A plain create names a known preset (`validate_preset_name`, folded). -/
abbrev presetCreateOk (r : Request) : Prop := r.preset ≠ "" ∧ presetKnown r

/-- A cloning create names an enabled source behavior. Payload cloning follows
optional behavior → context → tools references below this lifecycle projection;
omitted optional references use defaults; dangling configured references fail
the common loader. -/
abbrev cloneOk (st : BehaviorCatalog) (r : Request) : Prop :=
  r.preset = "" ∧ (r.cloneFrom, true) ∈ st.behaviors

/-- `create` splits on whether `cloneFrom` is empty. -/
abbrev createModeOk (st : BehaviorCatalog) (r : Request) : Prop :=
  (r.cloneFrom = "" ∧ presetCreateOk r) ∨ (r.cloneFrom ≠ "" ∧ cloneOk st r)

/-- The target behavior exists (with either enabled flag) — the `edit` /
`disable` `contains_key` check. -/
abbrev behaviorPresent (st : BehaviorCatalog) (id : String) : Prop :=
  (id, true) ∈ st.behaviors ∨ (id, false) ∈ st.behaviors

/-- Product-owned configurators may be cloned but cannot be edited or disabled
through their own sibling-persona tool. This keeps a recovery/configuration
behavior available while allowing newly created working behaviors to become
the principal default. -/
abbrev behaviorMutable (st : BehaviorCatalog) (id : String) : Prop :=
  id ∉ st.protectedIds

/-- Edit may preserve the current context (empty preset) or name a known
preset. Optional context/tools references are resolved by the common loader. -/
abbrev editNameOk (r : Request) : Prop :=
  match r.nameEdit with
  | .omitted | .clear => True
  | .set name => name ≠ ""

abbrev editRootOk (cat : Catalog) (r : Request) : Prop :=
  match r.rootEdit with
  | .omitted | .clear => True
  | .set root => root ∈ cat.roots

abbrev editProfileOk (cat : Catalog) (r : Request) : Prop :=
  match r.profileEdit with
  | .omitted => True
  | .clear => False
  | .set profile => profile.trim ≠ "" ∧ profile ∈ cat.profiles

abbrev editPromptOk (r : Request) : Prop :=
  match r.systemPromptEdit with
  | .omitted | .clear => True
  | .set prompt => prompt.trim ≠ ""

/-- Presets materialize Tools authority but are not themselves a persisted
field, so omission preserves Tools, a named preset replaces it, and a request
to clear a non-existent stored preset is rejected. -/
abbrev editPresetOk (r : Request) : Prop :=
  match r.presetEdit with
  | .omitted => True
  | .clear => False
  | .set preset => preset = "readonly" ∨ preset = "write"

instance (r : Request) : Decidable (editNameOk r) := by
  unfold editNameOk
  cases r.nameEdit <;> infer_instance

instance (cat : Catalog) (r : Request) : Decidable (editRootOk cat r) := by
  unfold editRootOk
  cases r.rootEdit <;> infer_instance

instance (cat : Catalog) (r : Request) : Decidable (editProfileOk cat r) := by
  unfold editProfileOk
  cases r.profileEdit <;> infer_instance

instance (r : Request) : Decidable (editPromptOk r) := by
  unfold editPromptOk
  cases r.systemPromptEdit <;> infer_instance

instance (r : Request) : Decidable (editPresetOk r) := by
  unfold editPresetOk
  cases r.presetEdit <;> infer_instance

/-- The request's `agent_did` names a known (enabled) principal in this
scope. The reconciler builds `Catalog.agents` from local enabled
`AgentPrincipal` rows, so a request for a phantom or foreign agent is
rejected instead of minting orphan behaviors/tools. -/
abbrev agentOk (cat : Catalog) (r : Request) : Prop := r.agent ∈ cat.agents

/-- Every generation component must match one unique current enrollment.
Missing authority, revocation, supersession, and cross-network replay all fail
closed. -/
def enrollmentAuthorizationOk (cat : Catalog) (r : Request) : Prop :=
  ∃ authorization,
    cat.authorization = some authorization ∧
    authorization.network = r.network ∧
    authorization.memberDid = r.requester ∧
    authorization.memberPeer = r.memberPeer ∧
    authorization.ownerAgent = r.agent ∧
    authorization.requestDigest = r.enrollmentRequestDigest ∧
    authorization.sequence = r.authorizationSequence ∧
    authorization.authorizationExpiresAt = r.authorizationExpiresAt

instance (cat : Catalog) (r : Request) : Decidable (enrollmentAuthorizationOk cat r) := by
  unfold enrollmentAuthorizationOk
  infer_instance

/-- Local persona management is a separate, explicitly tagged authority. The
request must be signed by the same local principal it requests and targets;
an unsigned row or any cross-branch field substitution fails closed. -/
def localSelfAuthorizationOk (r : Request) : Prop :=
  r.localSigner = r.requester ∧ r.requester = r.agent ∧ r.localSignatureValid = true

instance (r : Request) : Decidable (localSelfAuthorizationOk r) := by
  unfold localSelfAuthorizationOk
  infer_instance

def authorizationOk (cat : Catalog) (r : Request) : Prop :=
  (r.authorityKind = AuthorityKind.enrollment ∧ enrollmentAuthorizationOk cat r) ∨
  (r.authorityKind = AuthorityKind.localSelf ∧ localSelfAuthorizationOk r)

instance (cat : Catalog) (r : Request) : Decidable (authorizationOk cat r) := by
  unfold authorizationOk
  infer_instance

/-- The per-op admission conjuncts. -/
def opOk (cat : Catalog) (st : BehaviorCatalog) (r : Request) : Prop :=
  match r.op with
  | Op.create =>
      nameOk r ∧ createPromptOk r ∧ rootOk cat r ∧ profileOk cat r ∧ createModeOk st r
  | Op.edit =>
      behaviorPresent st r.target ∧ behaviorMutable st r.target ∧ editNameOk r ∧
        editRootOk cat r ∧ editProfileOk cat r ∧ editPromptOk r ∧ editPresetOk r
  | Op.disable =>
      behaviorPresent st r.target ∧ behaviorMutable st r.target ∧ r.makeDefault = false

instance (cat : Catalog) (st : BehaviorCatalog) (r : Request) : Decidable (opOk cat st r) := by
  unfold opOk
  cases r.op <;> infer_instance

/-- Admission gate, mirroring `decide_persona_request` conjunct-for-conjunct:
the agent must be known regardless of op, then the per-op conjuncts apply. -/
def admits (cat : Catalog) (st : BehaviorCatalog) (r : Request) : Prop :=
  authorizationOk cat r ∧ agentOk cat r ∧ opOk cat st r

instance (cat : Catalog) (st : BehaviorCatalog) (r : Request) : Decidable (admits cat st r) := by
  unfold admits
  infer_instance

/-- Existing deterministic command target: creates use the request key;
edits and disables target the selected behavior. -/
def targetBehaviorId (r : Request) : String :=
  match r.op with
  | .create => r.key
  | .edit | .disable => r.target

/-- Default selection is part of the same admitted create/edit publication.
It never rewrites the configurator/source behavior; it only points the
principal at the separately materialized target. -/
def defaultBehaviorAfter (preDefault appliedBehavior : String) (r : Request) : String :=
  if r.op ≠ .disable ∧ r.makeDefault = true then appliedBehavior else preDefault

theorem requested_promotion_selects_applied_behavior
    (preDefault appliedBehavior : String) (r : Request)
    (hop : r.op ≠ .disable) (hdefault : r.makeDefault = true) :
    defaultBehaviorAfter preDefault appliedBehavior r = appliedBehavior := by
  simp [defaultBehaviorAfter, hop, hdefault]

theorem omitted_promotion_keeps_existing_default
    (preDefault appliedBehavior : String) (r : Request)
    (hdefault : r.makeDefault = false) :
    defaultBehaviorAfter preDefault appliedBehavior r = preDefault := by
  simp [defaultBehaviorAfter, hdefault]

/-- A create/edit result must resolve as one context-plus-inference configuration.
The candidate is supplied by the common authoring loader, not reconstructed from
independent root/profile catalogs. Disable is admitted above but starts no session. -/
def materializedSession (cat : Catalog) (st : BehaviorCatalog) (r : Request)
    (candidate : Configuration.Registry) : Option Configuration.ResolvedSessionConfig :=
  if admits cat st r ∧ r.op ≠ .disable then
    (Configuration.resolveBehavior candidate r.agent (targetBehaviorId r)).toOption
  else none

/-- This boundary makes no claim about arbitrary host effects or payload copying:
it establishes authenticated command admission and actual canonical resolution. -/
theorem materializedSession_iff (cat : Catalog) (st : BehaviorCatalog) (r : Request)
    (candidate : Configuration.Registry) (session : Configuration.ResolvedSessionConfig) :
    materializedSession cat st r candidate = some session ↔
      admits cat st r ∧ r.op ≠ .disable ∧
      Configuration.resolveBehavior candidate r.agent (targetBehaviorId r) = .ok session := by
  unfold materializedSession
  by_cases h : admits cat st r ∧ r.op ≠ .disable
  · simp only [if_pos h]
    cases hr : Configuration.resolveBehavior candidate r.agent (targetBehaviorId r) <;>
      simp_all [Except.toOption]
  · simp only [if_neg h, reduceCtorEq]
    tauto

theorem unauthorized_command_resolves_nothing (cat : Catalog) (st : BehaviorCatalog)
    (r : Request) (candidate : Configuration.Registry)
    (h : ¬ authorizationOk cat r) : materializedSession cat st r candidate = none := by
  simp [materializedSession, admits, h]

theorem unknown_agent_resolves_nothing (cat : Catalog) (st : BehaviorCatalog)
    (r : Request) (candidate : Configuration.Registry)
    (h : r.agent ∉ cat.agents) : materializedSession cat st r candidate = none := by
  simp [materializedSession, admits, agentOk, h]

theorem disable_has_no_materialized_session (cat : Catalog) (st : BehaviorCatalog)
    (r : Request) (candidate : Configuration.Registry)
    (h : r.op = .disable) : materializedSession cat st r candidate = none := by
  simp [materializedSession, h]

/-- Local self-configuration retains exact-principal signature admission. -/
theorem unsigned_local_command_denied (r : Request) (cat : Catalog)
    (hkind : r.authorityKind = .localSelf) (hsig : r.localSignatureValid = false) :
    ¬ authorizationOk cat r := by
  simp [authorizationOk, hkind, localSelfAuthorizationOk, hsig]

theorem cross_principal_local_command_denied (r : Request) (cat : Catalog)
    (hkind : r.authorityKind = .localSelf) (hcross : r.requester ≠ r.agent) :
    ¬ authorizationOk cat r := by
  simp [authorizationOk, hkind, localSelfAuthorizationOk, hcross]

/-- Every admitted create (including clone) names a nonblank published
profile. Edits may omit the profile and preserve the stored binding. -/
theorem admitted_create_profile (cat : Catalog) (st : BehaviorCatalog) (r : Request)
    (hadm : admits cat st r) (hop : r.op = .create) :
    r.profile.trim ≠ "" ∧ r.profile ∈ cat.profiles := by
  have h := hadm.2.2
  simp [opOk, hop] at h
  exact h.2.2.2.1

theorem blank_create_profile_rejected (cat : Catalog) (st : BehaviorCatalog) (r : Request)
    (hop : r.op = .create) (hblank : r.profile.trim = "") :
    ¬ admits cat st r := by
  intro hadm
  exact (admitted_create_profile cat st r hadm hop).1 hblank

def asNameOnlyEdit (r : Request) (name : String) : Request :=
  let r := { r with op := .edit }
  let r := { r with nameEdit := .set name }
  let r := { r with descriptionEdit := .omitted }
  let r := { r with rootEdit := .omitted }
  let r := { r with profileEdit := .omitted }
  let r := { r with systemPromptEdit := .omitted }
  { r with presetEdit := .omitted }

def asProfileOnlyEdit (r : Request) (profile : String) : Request :=
  let r := { r with profileEdit := .set profile }
  let r := { r with nameEdit := .omitted }
  let r := { r with descriptionEdit := .omitted }
  let r := { r with systemPromptEdit := .omitted }
  let r := { r with rootEdit := .omitted }
  { r with presetEdit := .omitted }

theorem name_only_edit_does_not_require_profile (cat : Catalog) (st : BehaviorCatalog)
    (r : Request) (name : String) (htarget : behaviorPresent st r.target)
    (hmutable : behaviorMutable st r.target) (hname : name ≠ "") :
    opOk cat st (asNameOnlyEdit r name) := by
  simp [opOk, editNameOk, editRootOk, editProfileOk, editPromptOk,
    editPresetOk, asNameOnlyEdit, htarget, hmutable, hname]
  exact hmutable

theorem profile_only_edit_preserves_other_fields (profile : String)
    (r : Request) (storedName storedDescription storedPrompt storedRoot : Option String) :
    let edit := asProfileOnlyEdit r profile
    edit.nameEdit.apply storedName = storedName ∧
      edit.descriptionEdit.apply storedDescription = storedDescription ∧
      edit.systemPromptEdit.apply storedPrompt = storedPrompt ∧
      edit.rootEdit.apply storedRoot = storedRoot := by
  simp [FieldUpdate.apply, asProfileOnlyEdit]

/-- An admitted preset-based create cannot materialize an instructionless
working behavior. Clones retain the source prompt unless explicitly
overridden by the authoring loader. -/
theorem admitted_preset_create_has_prompt (cat : Catalog) (st : BehaviorCatalog) (r : Request)
    (hadm : admits cat st r) (hop : r.op = .create) (hclone : r.cloneFrom = "") :
    r.systemPrompt.trim ≠ "" := by
  have hopOk := hadm.2.2
  simp [opOk, createPromptOk, hop, hclone] at hopOk
  exact hopOk.2.1

theorem protected_edit_or_disable_rejected (cat : Catalog) (st : BehaviorCatalog) (r : Request)
    (hop : r.op = .edit ∨ r.op = .disable) (hprotected : r.target ∈ st.protectedIds) :
    ¬ admits cat st r := by
  intro hadm
  have hopOk := hadm.2.2
  rcases hop with h | h <;> simp [opOk, behaviorMutable, h, hprotected] at hopOk

end PersonaRequest
end PeerRegistryDiscovery
