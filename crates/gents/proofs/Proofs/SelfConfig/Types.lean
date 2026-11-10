import Proofs.ConfigDocuments

namespace SelfConfig

abbrev Target := ConfigDocuments.Collection
/-- Pack-owned callback modules, keys, and placements do not
become writable merely because the shared catalog knows about them. `AgentTarget`
is a `tools` document (#2058). -/
def allTargets : List Target :=
  [.agent, .agentContext, .compaction, .tools, .agentTarget, .inferenceProfile,
   .inferenceSampling, .inferenceExecution, .inferenceRetryPolicy,
   .inferenceBackend, .toolServiceRegistry, .task, .schedule, .trigger, .eventSource,
   .datastoreToolSurface, .skill]
abbrev Target.collectionName (t : Target) := ConfigDocuments.Collection.collectionName t
abbrev Target.uniqueField (t : Target) := ConfigDocuments.Collection.uniqueField t
abbrev Target.category (t : Target) := ConfigDocuments.Collection.category t

/-- Every self-config category. `node` is the agent catalog: the grant that
reaches another Agent's configuration and shared documents
(`SelfConfig.requiredCategories`). `policy` exposes the policy resource and is
held-bounded (`SelfConfig.Grants.policy`). The Engineer holds exactly this list
(`SelfConfig.engineerGroup`); the Rust `SELF_CONFIG_CATEGORIES` and the
Engineer preset are checked against it. -/
def selfConfigCategories : List String :=
  ["node", "agent", "tools", "profile", "backend", "mcp_service", "automation", "policy"]
/-- The categories an enabled group selects when its list is absent: own
configuration only, never the catalog or `policy`, so enabling self-config
without naming categories cannot reach another Agent or delegate grants. -/
def defaultCategories : List String := ["agent", "tools", "profile"]

theorem default_categories_exclude_catalog_and_policy :
    "node" ∉ defaultCategories ∧ "policy" ∉ defaultCategories := by decide

/-- The `policy` resource (`get`, `check`, `explain`, `grant`, `revoke`) is
exposed exactly when its category is selected. Admission of each grant is
decided by `Identity.GrantSets.policyToolAdmits`, never by the category alone. -/
def policyResourceExposed (categories : List String) : Bool :=
  categories.contains "policy"

theorem policy_resource_needs_category :
    policyResourceExposed ["agent", "tools", "node"] = false := by decide

theorem policy_resource_exposed_with_category :
    policyResourceExposed ["policy"] = true := by decide

/-- A resource category exposes its read commands. Document access remains
subject to native authorization for the invoking actor. -/
def resourceReadExposed (categories : List String) (target : Target) : Bool :=
  categories.contains target.category

/-- An enabled config tool exposes its own effective read; native document
authorization still determines which selected documents are visible. -/
def effectiveReadExposed (_categories : List String) : Bool := true

theorem backend_read_does_not_require_agent_catalog :
    resourceReadExposed ["backend"] .inferenceBackend = true := by decide

abbrev FieldKey := String
abbrev allFields := ConfigDocuments.Collection.fields

/-- Identity/provenance and module material are operator-managed; nested auth
references stay editable and raw-key/OAuth-account protection is enforced by
the Rust `validate` typed auth guard. -/
def protectedKey (t : Target) (k : FieldKey) : Bool :=
  (t == .task && k == "agent_did") || (t == .agent && k == "creation_nonce") ||
    k == t.uniqueField || ["node_did", "created_at", "updated_at", "created_by",
    "artifact_bytes", "canonical_args", "signer_did", "provenance"].contains k

def writableFields (t : Target) : List FieldKey :=
  if t ∈ allTargets then (allFields t).filter (fun k => !protectedKey t k) else []
def protectedFields (t : Target) : List FieldKey :=
  (allFields t).filter (fun k => decide (k ∉ writableFields t))

theorem writable_subset_all (t : Target) (_ht : t ∈ allTargets)
    (k : FieldKey) (hk : k ∈ writableFields t) : k ∈ allFields t :=
  by
    simp only [writableFields, if_pos _ht] at hk
    exact (List.mem_filter.mp hk).1

theorem all_fields_nodup : ∀ t ∈ allTargets, (allFields t).Nodup := by decide

theorem writable_fields_nodup : ∀ t ∈ allTargets, (writableFields t).Nodup := by decide

theorem unique_field_protected :
    ∀ t ∈ allTargets, t.uniqueField ∈ protectedFields t := by decide

theorem node_did_never_writable :
    ∀ t ∈ allTargets, "node_did" ∉ writableFields t := by decide

theorem agent_creation_nonce_protected :
    "creation_nonce" ∈ protectedFields .agent ∧
    "creation_nonce" ∉ writableFields .agent := by decide

theorem auth_reference_editable : "auth" ∈ writableFields .inferenceBackend := by decide

theorem datastore_entries_editable :
    "entries" ∈ writableFields .datastoreToolSurface := by decide

theorem skill_instructions_editable : "instructions" ∈ writableFields .skill := by decide

theorem datastore_owner_and_identity_protected :
    "node_did" ∉ writableFields .datastoreToolSurface ∧
    "surface_id" ∉ writableFields .datastoreToolSurface := by decide

theorem observations_not_writable :
    ∀ t ∈ allTargets, ∀ k ∈ ["probe_status", "last_probe", "fire_count",
      "next_run_at", "publication_status", "published_at"],
      k ∉ writableFields t := by decide

theorem recreate_identity_field_protected :
    ∀ t ∈ allTargets,
      "updated_at" ∈ allFields t → "updated_at" ∈ protectedFields t := by decide

theorem categories_well_formed :
    (∀ t ∈ allTargets, t.category ∈ selfConfigCategories)
      ∧ ∀ c ∈ defaultCategories, c ∈ selfConfigCategories := by decide

end SelfConfig
