import Proofs.Configuration
import Proofs.ConfigDefaults
import Lean

namespace Conformance.ConfigurationScope
open Configuration Lean

/-- Two principals reuse every authored label, with different context/model
payloads. The third scope is absent; an ill-owned decoded row remains rejected. -/
private def registry : Registry :=
  { tasks := fun owner id => if owner ∈ ["alice", "bob"] && id == "task"
      then some ⟨owner, ⟨"coding", true⟩⟩ else none
    behaviors := fun owner id => if owner ∈ ["alice", "bob"] && id == "coding"
      then some ⟨owner, ⟨some "context", "profile", true⟩⟩ else none
    contexts := fun owner id => if owner ∈ ["alice", "bob"] && id == "context"
      then some ⟨owner, { instructions := owner }⟩ else none
    profiles := fun owner id => if owner ∈ ["alice", "bob"] && id == "profile"
      then some ⟨owner, ⟨"backend", owner ++ "-model", none⟩⟩ else none
    backends := fun owner id => if owner ∈ ["alice", "bob"] && id == "backend"
      then some ⟨owner, ⟨true⟩⟩ else none }

example : (resolveTask registry "alice" "task").toOption.map (·.context.instructions) =
    some "alice" := by rfl
example : (resolveTask registry "bob" "task").toOption.map (·.inference.model) =
    some "bob-model" := by rfl
example : resolveTask registry "absent" "task" = .error .missingTask := by rfl

/-! Generated naming witnesses cover authoring grammar separately from stored
key compatibility and from DID/document ownership. -/

example : validGeneratedQualifiedKey "local:reviewer" = true := by native_decide
example : validGeneratedQualifiedKey "gents:base:configurator:retry-policy" = true := by
  native_decide
example : validGeneratedQualifiedKey "Local:reviewer" = false := by native_decide
example : validGeneratedQualifiedKey "local:review_er" = false := by native_decide
example : validGeneratedQualifiedKey "local::reviewer" = false := by native_decide
example : validGeneratedQualifiedKey "local:-reviewer" = false := by native_decide
example : validGeneratedQualifiedKey "local:reviewer--worker" = false := by native_decide
example : validNewPersonalBehaviorKey "local:reviewer" = true := by native_decide
example : validNewPersonalBehaviorKey "gents:base:configurator" = false := by native_decide
example : personalBehaviorKey "reviewer" = "local:reviewer" := by native_decide
example : personalBehaviorKey "reviewer" 2 = "local:reviewer-2" := by native_decide
example : personalBehaviorKey "reviewer" 3 = "local:reviewer-3" := by native_decide
example :
    behaviorComponentId "local:reviewer-2" .context = "local:reviewer-2:context" := by
  native_decide
example :
    behaviorComponentId "local:reviewer-2" .compactionRetryPolicy =
      "local:reviewer-2:compaction:retry-policy" := by native_decide
example :
    (generatedPersonalBehaviorNames "Jack's reviewer" "reviewer" 1).behaviorId =
      (generatedPersonalBehaviorNames "レビュー担当" "reviewer" 1).behaviorId := by
  native_decide
example : keyAccepted .newGenerated "Legacy_Reviewer" = false := by native_decide
example : keyAccepted .newPersonal "local:reviewer-2" = true := by native_decide
example : keyAccepted .retainedStored "Legacy_Reviewer" = true := by native_decide

private def namingValidationCase (key : String) : Json :=
  Json.mkObj [("key", toJson key), ("valid", toJson (validGeneratedQualifiedKey key))]

private def componentCase (path : BehaviorComponentPath) : Json :=
  Json.mkObj
    [("suffix", toJson path.suffix),
     ("id", toJson (behaviorComponentId "local:reviewer-2" path))]

private def namingCasesJson : Json := Json.mkObj
  [("validation", toJson
      (["local:reviewer", "gents:base:configurator:retry-policy", "Local:reviewer",
        "local:review_er", "local::reviewer", "local:-reviewer",
        "local:reviewer--worker"].map namingValidationCase)),
   ("personal_allocations", toJson
      ([1, 2, 3].map fun ordinal => Json.mkObj
        [("ordinal", toJson ordinal),
         ("behavior_id", toJson (personalBehaviorKey "reviewer" ordinal))])),
   ("components", toJson
      ([.context, .tools, .inference, .sampling, .execution, .retryPolicy, .compaction,
        .compactionInference, .compactionSampling, .compactionExecution,
        .compactionRetryPolicy].map componentCase)),
   ("display_name_independent", toJson
      ((generatedPersonalBehaviorNames "Jack's reviewer" "reviewer" 1).behaviorId ==
       (generatedPersonalBehaviorNames "レビュー担当" "reviewer" 1).behaviorId)),
   ("legacy_stored_key_accepted", toJson
      (keyAccepted .retainedStored "Legacy_Reviewer")),
   ("new_personal_key_valid", toJson
      (keyAccepted .newPersonal (personalBehaviorKey "reviewer" 2)))]

/-! Behavior-owned configuration fixtures exercise the canonical
`scope_behavior_id` contract independently of principal ownership. -/

private def scopedRegistry : ScopedConfigRegistry :=
  { behaviors := [⟨"coding", some "context", "inference"⟩]
    contexts :=
      [{ contextId := "context", scopeBehaviorId := some "coding",
         toolsId := some "tools", compactionId := some "compaction",
         skillIds := ["shared-skill"] }]
    tools :=
      [{ toolsId := "tools", scopeBehaviorId := some "coding",
         mcpServiceIds := ["shared-mcp"], subagentTargetIds := ["shared-subagent"],
         datastoreSurfaceIds := ["shared-datastore"], integrationIds := ["shared-integration"] }]
    compactions :=
      [{ compactionId := "compaction", scopeBehaviorId := some "coding",
         inferenceProfileId := some "compaction-inference" }]
    profiles :=
      [{ profileId := "inference", scopeBehaviorId := some "coding",
         backendId := "shared-backend", samplingId := some "sampling",
         executionId := some "execution" },
       { profileId := "compaction-inference", scopeBehaviorId := some "coding",
         backendId := "shared-backend", samplingId := some "compaction-sampling",
         executionId := some "compaction-execution" }]
    samplings :=
      [⟨"sampling", some "coding"⟩, ⟨"compaction-sampling", some "coding"⟩]
    executions :=
      [{ executionId := "execution", scopeBehaviorId := some "coding",
         retryPolicyId := some "retry" },
       { executionId := "compaction-execution", scopeBehaviorId := some "coding" }]
    retryPolicies := [⟨"retry", some "coding"⟩] }

private def legacyRegistry : ScopedConfigRegistry :=
  { scopedRegistry with
    contexts := scopedRegistry.contexts.map fun document =>
      { document with scopeBehaviorId := none }
    , tools := scopedRegistry.tools.map fun document =>
      { document with scopeBehaviorId := none }
    , compactions := scopedRegistry.compactions.map fun document =>
      { document with scopeBehaviorId := none }
    , profiles := scopedRegistry.profiles.map fun document =>
      { document with scopeBehaviorId := none }
    , samplings := scopedRegistry.samplings.map fun document =>
      { document with scopeBehaviorId := none }
    , executions := scopedRegistry.executions.map fun document =>
      { document with scopeBehaviorId := none }
    , retryPolicies := scopedRegistry.retryPolicies.map fun document =>
      { document with scopeBehaviorId := none } }

private def mixedRegistry : ScopedConfigRegistry :=
  { scopedRegistry with
    tools := scopedRegistry.tools.map fun document =>
      { document with scopeBehaviorId := none } }

private def orphanRegistry : ScopedConfigRegistry :=
  { scopedRegistry with
    retryPolicies := ⟨"orphan-retry", some "coding"⟩ :: scopedRegistry.retryPolicies }

private def missingBehaviorRegistry : ScopedConfigRegistry :=
  { scopedRegistry with
    contexts := ⟨"absent-context", some "absent", none, none, []⟩ :: scopedRegistry.contexts }

private def crossBehaviorAliasRegistry : ScopedConfigRegistry :=
  { scopedRegistry with
    behaviors := ⟨"other", none, "inference"⟩ :: scopedRegistry.behaviors }

example : behaviorScopesValid scopedRegistry = true := by native_decide
example : behaviorScopesValid legacyRegistry = true := by native_decide
example : behaviorScopesValid mixedRegistry = false := by native_decide
example : behaviorScopesValid orphanRegistry = false := by native_decide
example : behaviorScopesValid missingBehaviorRegistry = false := by native_decide
example : behaviorScopesValid crossBehaviorAliasRegistry = false := by native_decide

/-- Changing reusable-resource references does not enter the owned closure. -/
example :
    behaviorScopesValid
      { scopedRegistry with
        tools := scopedRegistry.tools.map fun document =>
          { toolsId := document.toolsId, scopeBehaviorId := document.scopeBehaviorId,
            mcpServiceIds := ["other-mcp"], subagentTargetIds := ["other-subagent"],
            datastoreSurfaceIds := ["other-datastore"],
            integrationIds := ["other-integration"] }
        , contexts := scopedRegistry.contexts.map fun document =>
          { document with skillIds := ["other-skill"] }
        , profiles := scopedRegistry.profiles.map fun document =>
          { document with backendId := "other-shared-backend" } } = true := by native_decide

private def scopeCaseJson (name : String) (reg : ScopedConfigRegistry) : Json :=
  Json.mkObj [("name", toJson name), ("valid", toJson (behaviorScopesValid reg))]

private def scopeCasesJson : Json := toJson
  [scopeCaseJson "complete_scoped_closure" scopedRegistry,
   scopeCaseJson "complete_legacy_unscoped_closure" legacyRegistry,
   scopeCaseJson "mixed_scoped_and_unscoped_rejected" mixedRegistry,
   scopeCaseJson "scoped_orphan_rejected" orphanRegistry,
   scopeCaseJson "missing_named_behavior_rejected" missingBehaviorRegistry,
   scopeCaseJson "cross_behavior_alias_rejected" crossBehaviorAliasRegistry]

private def caseJson (owner : String) : Json := Json.mkObj
  [("owner", toJson owner), ("task_id", toJson "task"),
   ("result", (resolveTask registry owner "task").toOption.map (fun s => Json.mkObj
      [("instructions", toJson s.context.instructions), ("model", toJson s.inference.model),
       ("backend_id", toJson s.inference.backendId)]) |>.getD Json.null)]

private def contextBoundsCases : List (Option Int × Option Int × Option Int) := [
  (some 872000, some 272000, some 872000),
  (some 872001, some 272000, some 872000),
  (some 872001, some 272000, none),
  (some 872001, some 272000, some 0),
  (some 872001, some 272000, some 128000),
  (none, some 272000, some 872000),
  (some 872001, none, some 872000),
  (some 872001, some (-1), some 872000)]

private def contextBoundsJson : Json := toJson (contextBoundsCases.map fun (selected, modelDefault, maximum) =>
  Json.mkObj [("selected", toJson selected), ("default", toJson modelDefault),
    ("maximum", toJson maximum),
    ("allowed", toJson (ConfigDefaults.contextOverrideAllowed selected modelDefault maximum))])

/-- Export the actual shared-label input documents as well as computed results. -/
def casesJson : String := (Json.mkObj
  [("documents", toJson (["alice", "bob"].map fun owner => Json.mkObj
      [("agent_did", toJson owner), ("task_id", toJson "task"),
       ("behavior_id", toJson "coding"), ("context_id", toJson "context"),
       ("profile_id", toJson "profile"), ("backend_id", toJson "backend"),
       ("instructions", toJson owner), ("model", toJson (owner ++ "-model")),
       ("enabled", toJson true)])),
   ("cases", toJson (["alice", "bob", "absent"].map caseJson)),
   ("naming_cases", namingCasesJson),
   ("behavior_scope_cases", scopeCasesJson),
   ("context_bounds", contextBoundsJson)]).compress

/-- These scope fixtures advertise one model with unknown capabilities. Backend
owner comes from the containing document, independently of credential scope. -/
private def advertisement (credentials : Option String) : DiscoveryObservation :=
  ⟨"backend", credentials, [⟨"advertised", none⟩]⟩

example : catalogFor "alice" "backend" none (some ⟨"bob", advertisement none⟩) = none := by rfl
example : catalogFor "alice" "backend" none (some ⟨"alice", advertisement none⟩) =
    some (advertisement none) := by rfl

private def discoveryCaseJson (name requestedOwner observedOwner requestedBackend : String)
    (requestedCredentials observedCredentials : Option String) : Json :=
  let observed := advertisement observedCredentials
  let result := catalogFor requestedOwner requestedBackend requestedCredentials (some ⟨observedOwner, observed⟩)
  let catalogJson := fun (c : DiscoveryObservation) => Json.mkObj
    [("backend_id", toJson c.backendId), ("credential_principal", toJson c.agentDid),
     ("model_names_with_unknown_capabilities", toJson (c.models.map (·.modelName)))]
  Json.mkObj [("name", toJson name), ("requested_owner", toJson requestedOwner),
    ("requested_backend", toJson requestedBackend), ("requested_credentials", toJson requestedCredentials),
    ("observed_owner", toJson observedOwner), ("observed", catalogJson observed),
    ("result", result.map catalogJson |>.getD Json.null)]

def discoveryCasesJson : String := (toJson [
  discoveryCaseJson "same_owner_shared_credentials" "alice" "alice" "backend" none none,
  discoveryCaseJson "foreign_owner_same_label_shared_credentials" "alice" "bob" "backend" none none,
  discoveryCaseJson "wrong_backend" "alice" "alice" "other" none none,
  discoveryCaseJson "matching_oauth_scope" "alice" "alice" "backend" (some "alice") (some "alice"),
  discoveryCaseJson "foreign_oauth_scope" "alice" "alice" "backend" (some "alice") (some "bob"),
  discoveryCaseJson "oauth_is_not_shared_credentials" "alice" "alice" "backend" none (some "alice"),
  discoveryCaseJson "shared_catalog_is_not_oauth" "alice" "alice" "backend" (some "alice") none,
  discoveryCaseJson "oauth_catalog_still_requires_backend_owner" "alice" "bob" "backend" (some "alice") (some "alice")
  ]).compress

end Conformance.ConfigurationScope
