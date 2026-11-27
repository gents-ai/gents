import Lean
import Proofs.Store.BackendPreflight

/-!
The 1.0 store migration contract: lineage identification, release change
classification, signer authority, staged crash-safe execution, and policy ID
transitions across post-1.0 releases.

Decision on `gents-migration`: retired (deleted from the workspace). The legacy
crate drove in-place DefraDB schema patches that could not carry access-policy
ID changes or document ownership transitions across releases and risked partial
in-place corruption on crash. Baseline schema registration, ordinary index
reconciliation, and read-only compatibility checks live in
`gents_store::schema::catalog` and `gents_store::local::store_compat`; post-1.0
migrations follow the staged, verified, copy-and-promote contract modelled here
and owned by `gents_store::local::store_migration`.

Pre-1.0 stores are never converted: `migratableFrom` is `false` for 1.0, so any
older, unmarked, or unknown pre-1.0 store is refused read-only before taking
the store lock and left byte-untouched.
-/

namespace StoreMigration

/-- What the store's typed manifest file (`.gents-store-manifest.json`) in the
data directory reports when read before opening DefraDB and before taking the
store lock. -/
inductive ManifestState where
  | absent
  | matching
  | olderContract
  | newerContract
  | corruptOrMismatched
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- What a runtime decides when inspecting a store directory before opening it.
Every refusal leaves the store unopened and byte-identical. -/
inductive OpenDisposition where
  | open
  | migrate
  | refuseOlder
  | refuseNewer
  | refuseCorrupt
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- Decide whether a store may open, requires a registered post-1.0 migration,
or must be refused without writing a byte.

`lineage` comes from `.gents-private-backend-v<N>` directory names
(`BackendPreflight.lineage`). `manifest` comes from `.gents-store-manifest.json`.
`migratableFrom` is `true` only when this release ships an explicit migration
path from the observed older lineage (`false` for 1.0). `beforeLock` is `true`
when read before taking `data.lock`: an unmarked directory with an absent or
matching manifest may still be mid-initialization under another process's lock,
because initialization writes `.gents-store-manifest.json` before the commit-point
`.gents-private-backend-v<N>` marker. -/
def openDisposition (lineage : BackendPreflight.Lineage) (manifest : ManifestState)
    (migratableFrom : Bool) (beforeLock : Bool) : OpenDisposition :=
  match lineage, manifest with
  | .newer, _ => .refuseNewer
  | _, .newerContract => .refuseNewer
  | _, .corruptOrMismatched => .refuseCorrupt
  | .current, .matching => .open
  | .current, .absent => .refuseOlder
  | .current, .olderContract => .refuseOlder
  | .older, .olderContract => if migratableFrom then .migrate else .refuseOlder
  | .older, .absent => .refuseOlder
  | .older, .matching => .refuseCorrupt
  | .unmarked, .absent => if beforeLock then .open else .refuseOlder
  | .unmarked, .matching => if beforeLock then .open else .refuseOlder
  | .unmarked, .olderContract =>
      if beforeLock && migratableFrom then .migrate else .refuseOlder

/-- Kinds of schema, access-policy, ownership, or identity changes a release
may introduce relative to the 1.0 store baseline. -/
inductive ChangeKind where
  | newCollection
  | newOrdinaryIndex
  | additiveNullableField
  | breakingSchemaChange
  | policyIdOrRuleChange
  | documentOwnershipOrResourceChange
  | identityDerivationChange
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- How a release carries a change kind from a 1.0 or later store. -/
inductive ChangeHandling where
  /-- Applied directly on boot by `schema::catalog::ensure` without a staged
  migration or lineage bump. -/
  | inPlaceOnBoot
  /-- Applied only in an isolated staged copy under the exclusive store lock,
  verified before retiring the source, and promoted atomically with a lineage
  bump. -/
  | stagedMigration
  /-- Never migrated across releases: invalidates signed history, ACP owners,
  and peer enrollment; requires a fresh home. -/
  | forbidden
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- Which DID actor signs a migration step. Agents hold only `agentBaseline`
permissions and never sign store migrations. The Node holds `nodeRuntimePermissions`
(which include `patch-collection` and `new-index` but exclude `bypass-dac`).
Only the User (home NAC owner / administrator) holds authority across all
actors' documents, policy bindings, and ownership changes. -/
inductive MigrationSigner where
  | node
  | user
  | none
  deriving BEq, DecidableEq, Repr, Lean.ToJson

structure ChangeRule where
  handling : ChangeHandling
  signer : MigrationSigner
  bumpsLineage : Bool
  deriving BEq, DecidableEq, Repr, Lean.ToJson

def changeRule (kind : ChangeKind) : ChangeRule :=
  match kind with
  | .newCollection =>
      { handling := .inPlaceOnBoot, signer := .node, bumpsLineage := false }
  | .newOrdinaryIndex =>
      { handling := .inPlaceOnBoot, signer := .node, bumpsLineage := false }
  | .additiveNullableField =>
      { handling := .stagedMigration, signer := .node, bumpsLineage := true }
  | .breakingSchemaChange =>
      { handling := .stagedMigration, signer := .user, bumpsLineage := true }
  | .policyIdOrRuleChange =>
      { handling := .stagedMigration, signer := .user, bumpsLineage := true }
  | .documentOwnershipOrResourceChange =>
      { handling := .stagedMigration, signer := .user, bumpsLineage := true }
  | .identityDerivationChange =>
      { handling := .forbidden, signer := .none, bumpsLineage := false }

/-- Durable journal phases for a crash-safe, resumable staged migration
(`data.migration.json`), held under the exclusive store lock. -/
inductive MigrationPhase where
  | staging
  | verified
  | promoted
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- The single atomic next step of a staged migration given its durable journal
phase and the presence of the live `source` (`data`), sibling `stage`
(`data.migration-stage`), and `retired` (`data.migration-source`) directories. -/
inductive MigrationAction where
  | stageAndTransform
  | retireSource
  | promoteStage
  | recordPromoted
  | verifyAndFinish
  | refuse
  deriving BEq, DecidableEq, Repr, Lean.ToJson

def migrationStep (phase : MigrationPhase) (source stage retired : Bool) : MigrationAction :=
  match phase, source, stage, retired with
  | .staging, true, _, false => .stageAndTransform
  | .verified, true, true, false => .retireSource
  | .verified, false, true, true => .promoteStage
  | .verified, true, false, true => .recordPromoted
  | .promoted, true, false, _ => .verifyAndFinish
  | _, _, _, _ => .refuse

/-- The retired original store and migration journal may be removed only after
the migration is durably recorded as `promoted` and the target release's
opening checks accept the promoted store. -/
def mayFinish (phase : MigrationPhase) (targetAccepted : Bool) : Bool :=
  phase == .promoted && targetAccepted

/-- Where document ACP runs for the store being migrated. -/
inductive AcpProvider where
  | local
  | vera
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- How a staged store acquires the target built-in policy when a policy
changes across releases: local homes register the policy under counter 0 as the
Node, whereas Vera homes seed the policy in the staged deployment's genesis
block and adopt it (`DocumentAcpChoice.PolicyIds`). -/
inductive PolicyRegistration where
  | none
  | nodeCounter
  | veraSeed
  deriving BEq, DecidableEq, Repr, Lean.ToJson

/-- How a staged migration carries a built-in access policy or resource change
across releases without leaving existing documents unreadable.

External premise (pinned DefraDB): a collection's `@policy(id:, resource:)` is
bound into its collection version lineage and cannot be rewritten in place,
adding a modified policy at an existing counter/ID is refused, and every
document's ACP registration is keyed by `(policy_id, resource, doc_id)`.
Therefore any change to a policy ID, a policy's rules at the same ID, or a
collection's resource binding rebuilds a fresh staged Regolith store under the
target policy, recreates documents with preserved fields, and re-registers
document ownership and relationships under `(new_policy_id, new_resource, doc_id)`
signed by the User before retiring the source. -/
structure PolicyTransitionPlan where
  policyRegistration : PolicyRegistration
  rebindCollections : Bool
  reRegisterDocuments : Bool
  rebuildStagedStore : Bool
  signer : MigrationSigner
  deriving BEq, DecidableEq, Repr, Lean.ToJson

def policyTransition (policyIdChanged resourceChanged samePolicyIdRulesChanged : Bool)
    (provider : AcpProvider) : PolicyTransitionPlan :=
  let policyChanges := policyIdChanged || samePolicyIdRulesChanged
  let bindingChanges := policyChanges || resourceChanged
  if bindingChanges then
    let policyRegistration :=
      if policyChanges then
        match provider with
        | .local => .nodeCounter
        | .vera => .veraSeed
      else .none
    { policyRegistration
      rebindCollections := true
      reRegisterDocuments := true
      rebuildStagedStore := true
      signer := .user }
  else
    { policyRegistration := .none
      rebindCollections := false
      reRegisterDocuments := false
      rebuildStagedStore := false
      signer := .none }

theorem pre_1_0_never_migrates (l : BackendPreflight.Lineage) (m : ManifestState)
    (b : Bool) : openDisposition l m false b ≠ .migrate := by
  cases l <;> cases m <;> cases b <;> decide

theorem newer_lineage_always_refused_as_newer (m : ManifestState) (mig b : Bool) :
    openDisposition .newer m mig b = .refuseNewer := by
  cases m <;> cases mig <;> cases b <;> rfl

theorem newer_manifest_always_refused_as_newer (l : BackendPreflight.Lineage) (mig b : Bool) :
    openDisposition l .newerContract mig b = .refuseNewer := by
  cases l <;> cases mig <;> cases b <;> rfl

theorem current_with_matching_manifest_opens (mig b : Bool) :
    openDisposition .current .matching mig b = .open := by
  cases mig <;> cases b <;> rfl

theorem older_without_migration_is_refused (m : ManifestState) (b : Bool)
    (notNewer : m ≠ .newerContract) (notCorrupt : m ≠ .corruptOrMismatched)
    (notMatching : m ≠ .matching) :
    openDisposition .older m false b = .refuseOlder := by
  cases m <;> cases b <;> simp_all [openDisposition]

theorem corrupt_manifest_never_opens (l : BackendPreflight.Lineage) (mig b : Bool) :
    openDisposition l .corruptOrMismatched mig b ≠ .open := by
  cases l <;> cases mig <;> cases b <;> decide

theorem unmarked_under_lock_never_opens (m : ManifestState) (mig : Bool) :
    openDisposition .unmarked m mig false ≠ .open := by
  cases m <;> cases mig <;> decide

theorem identity_derivation_change_is_forbidden :
    (changeRule .identityDerivationChange).handling = .forbidden
      ∧ (changeRule .identityDerivationChange).signer = .none
      ∧ (changeRule .identityDerivationChange).bumpsLineage = false := by
  decide

theorem policy_and_ownership_changes_require_user_signer :
    (changeRule .policyIdOrRuleChange).signer = .user
      ∧ (changeRule .documentOwnershipOrResourceChange).signer = .user
      ∧ (changeRule .breakingSchemaChange).signer = .user := by
  decide

theorem boot_additions_require_no_lineage_bump (k : ChangeKind)
    (h : (changeRule k).handling = .inPlaceOnBoot) :
    (changeRule k).bumpsLineage = false ∧ (changeRule k).signer = .node := by
  cases k <;> simp_all [changeRule]

theorem staged_migrations_bump_lineage (k : ChangeKind)
    (h : (changeRule k).handling = .stagedMigration) :
    (changeRule k).bumpsLineage = true ∧ (changeRule k).signer ≠ .none := by
  cases k <;> simp_all [changeRule]

theorem staging_never_retires_or_promotes (s st r : Bool) :
    migrationStep .staging s st r ≠ .retireSource
      ∧ migrationStep .staging s st r ≠ .promoteStage
      ∧ migrationStep .staging s st r ≠ .verifyAndFinish := by
  cases s <;> cases st <;> cases r <;> decide

theorem promotion_requires_verification_and_retired_source (p : MigrationPhase)
    (s st r : Bool) (h : migrationStep p s st r = .promoteStage) :
    p = .verified ∧ s = false ∧ st = true ∧ r = true := by
  cases p <;> cases s <;> cases st <;> cases r <;> simp_all [migrationStep]

theorem source_or_retired_present_on_every_non_refusal (p : MigrationPhase)
    (s st r : Bool) (h : migrationStep p s st r ≠ .refuse) :
    s = true ∨ r = true := by
  cases p <;> cases s <;> cases st <;> cases r <;> simp_all [migrationStep]

theorem finish_requires_promoted_and_accepted (p : MigrationPhase) (accepted : Bool) :
    mayFinish p accepted = true ↔ p = .promoted ∧ accepted = true := by
  cases p <;> cases accepted <;> decide

theorem policy_change_requires_staged_rebuild_and_user_signer
    (idChanged resChanged rulesChanged : Bool) (provider : AcpProvider)
    (changed : idChanged = true ∨ resChanged = true ∨ rulesChanged = true) :
    let plan := policyTransition idChanged resChanged rulesChanged provider
    plan.rebindCollections = true
      ∧ plan.reRegisterDocuments = true
      ∧ plan.rebuildStagedStore = true
      ∧ plan.signer = .user := by
  cases idChanged <;> cases resChanged <;> cases rulesChanged <;> cases provider <;>
    simp_all [policyTransition]

theorem unchanged_policy_needs_no_rebuild (provider : AcpProvider) :
    policyTransition false false false provider =
      { policyRegistration := .none
        rebindCollections := false
        reRegisterDocuments := false
        rebuildStagedStore := false
        signer := .none } := by
  cases provider <;> rfl

theorem vera_policy_change_uses_seeded_registration (resChanged : Bool) :
    (policyTransition true resChanged false .vera).policyRegistration = .veraSeed
      ∧ (policyTransition false resChanged true .vera).policyRegistration = .veraSeed := by
  cases resChanged <;> decide

end StoreMigration
