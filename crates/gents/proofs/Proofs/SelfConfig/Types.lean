import Proofs.ConfigDocuments

namespace SelfConfig

abbrev Target := ConfigDocuments.Collection
/-- Existing self-config families plus their extracted subdocuments. Pack-owned
callback modules, keys, graph definitions, and placements do not become writable
merely because the shared catalog knows about them. -/
def allTargets : List Target :=
  [.agentBehavior, .agentContext, .compaction, .tools, .inferenceProfile,
   .inferenceSampling, .inferenceExecution, .inferenceRetryPolicy,
   .inferenceBackend, .toolServiceRegistry, .task, .schedule, .trigger, .eventSource]
abbrev Target.collectionName (t : Target) := ConfigDocuments.Collection.collectionName t
abbrev Target.uniqueField (t : Target) := ConfigDocuments.Collection.uniqueField t
abbrev Target.category (t : Target) := ConfigDocuments.Collection.category t

def selfConfigCategories : List String :=
  ["behavior", "tools", "profile", "backend", "mcp_service", "automation", "persona"]
def defaultCategories : List String := ["behavior", "tools", "profile"]
abbrev FieldKey := String
abbrev allFields := ConfigDocuments.Collection.fields

/-- Identity/provenance and module material are operator-managed. Nested auth
references remain editable; raw-key protection belongs to the typed guard. -/
def protectedKey (t : Target) (k : FieldKey) : Bool :=
  (t == .task && k == "behavior_id") || k == t.uniqueField || ["agent_did", "created_at", "updated_at", "created_by",
    "wasm_bytes", "canonical_args", "signer_did", "provenance"].contains k

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

theorem agent_did_never_writable :
    ∀ t ∈ allTargets, "agent_did" ∉ writableFields t := by decide

theorem auth_reference_editable : "auth" ∈ writableFields .inferenceBackend := by decide

/-- Observations cannot be introduced by a self-config patch. -/
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
