import Proofs.Configuration
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

private def caseJson (owner : String) : Json := Json.mkObj
  [("owner", toJson owner), ("task_id", toJson "task"),
   ("result", (resolveTask registry owner "task").toOption.map (fun s => Json.mkObj
      [("instructions", toJson s.context.instructions), ("model", toJson s.inference.model),
       ("backend_id", toJson s.inference.backendId)]) |>.getD Json.null)]

/-- Export the actual shared-label input documents as well as computed results. -/
def casesJson : String := (Json.mkObj
  [("documents", toJson (["alice", "bob"].map fun owner => Json.mkObj
      [("agent_did", toJson owner), ("task_id", toJson "task"),
       ("behavior_id", toJson "coding"), ("context_id", toJson "context"),
       ("profile_id", toJson "profile"), ("backend_id", toJson "backend"),
       ("instructions", toJson owner), ("model", toJson (owner ++ "-model")),
       ("enabled", toJson true)])),
   ("cases", toJson (["alice", "bob", "absent"].map caseJson))]).compress

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
