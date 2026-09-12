import Proofs.Configuration
import Proofs.Basic
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
  root : String
  preset : String
  profile : String
  makeDefault : Bool
  cloneFrom : String
  target : String
  deriving DecidableEq, Repr

/-- Read-only enabled/id projection for command target checks. Canonical references
and ownership are validated by resolution of the compiled candidate below. -/
structure BehaviorCatalog where
  behaviors : Finset (String × Bool)
  deriving DecidableEq

-- These admission conjuncts are `abbrev` (reducible) so the `Decidable`
-- instance for `admits` resolves through them via `infer_instance`.

/-- The two built-in preset names (`persona_presets::builtin_preset_names`). -/
abbrev presetKnown (r : Request) : Prop :=
  r.preset = "readonly" ∨ r.preset = "write"

abbrev nameOk (r : Request) : Prop := r.name ≠ ""

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

/-- Edit may preserve the current context (empty preset) or name a known
preset. Optional context/tools references are resolved by the common loader. -/
abbrev editPresetOk (r : Request) : Prop := r.preset = "" ∨ presetKnown r

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
      nameOk r ∧ rootOk cat r ∧ profileOk cat r ∧ createModeOk st r
  | Op.edit =>
      behaviorPresent st r.target ∧ nameOk r ∧ rootOk cat r ∧
        profileOk cat r ∧ editPresetOk r
  | Op.disable =>
      behaviorPresent st r.target ∧ r.makeDefault = false

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

/-- Every admitted create/edit (including clone) names a nonblank published
profile. Disable selects no inference and retains its separate admission rule. -/
theorem admitted_profile (cat : Catalog) (st : BehaviorCatalog) (r : Request)
    (hadm : admits cat st r) (hop : r.op ≠ .disable) :
    r.profile.trim ≠ "" ∧ r.profile ∈ cat.profiles := by
  have h := hadm.2.2
  cases he : r.op <;> simp_all [opOk]

theorem blank_profile_rejected (cat : Catalog) (st : BehaviorCatalog) (r : Request)
    (hop : r.op ≠ .disable) (hblank : r.profile.trim = "") :
    ¬ admits cat st r := by
  intro hadm
  exact (admitted_profile cat st r hadm hop).1 hblank

end PersonaRequest
end PeerRegistryDiscovery
