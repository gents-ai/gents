import Proofs.Identity.Household

namespace Identity.Configuration
open Household

/-- A configuration reference is application data, not a native ACP grant.
Creation alone installs neither a relationship to a selecting Agent nor a
grant to its actor, including when several Agents select the same document. -/
theorem create_does_not_install_selection_grants (state : State)
    (creator resource id : String) (authenticated nodeCreate : Bool) :
    (directCreate state creator resource id authenticated nodeCreate).relations = state.relations :=
  direct_create_has_no_implicit_relations state creator resource id authenticated nodeCreate

/-- The authenticated creator becomes the native owner. If an Agent creates
its own configuration, revoking a separate writer relation does not revoke its
ownership; a policy requiring user ownership must account for that create path. -/
theorem agent_created_configuration_retains_creator (state : State)
    (agent resource id : String)
    (fresh : state.objects.any (fun object => object.resource == resource && object.id == id) = false)
    (grant : Relation) :
    (apply (directCreate state agent resource id true true) (.revoke grant)).objects =
      ⟨resource, id, agent⟩ :: state.objects := by
  simp [apply, directCreate, fresh]

/-- DefraDB's policy directive applies to the collection, not individual
fields. Backend observation and operator configuration updates therefore ask
for the same document permission while they share InferenceBackend. -/
inductive BackendField where
  | configuration
  | observation
  deriving BEq, Repr

def backendUpdatePermission (_field : BackendField) : String := "update"

theorem backend_observation_and_configuration_have_same_permission :
    backendUpdatePermission .observation = backendUpdatePermission .configuration := by rfl

/-- A runtime reads the user-owned Node and its Agents through its own
explicit reader grant. The grant conveys no administration or session access. -/
def runtimeReadFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "agent", "user"⟩,
      ⟨"sessions", "session", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"agents", "agent", "parent", .object "nodes" "node"⟩,
      ⟨"sessions", "session", "parent", .object "agents" "agent"⟩],
    enrolled := [] }

theorem runtime_read_grant_is_limited :
    [allowed 5 runtimeReadFixture "runtime" "nodes" "node" "read",
     allowed 5 runtimeReadFixture "runtime" "agents" "agent" "read",
     allowed 5 runtimeReadFixture "runtime" "nodes" "node" "update",
     allowed 5 runtimeReadFixture "runtime" "sessions" "session" "read"] =
    [true, true, false, false] := by native_decide

theorem revoked_runtime_reader_loses_inherited_read :
    let revoked := apply runtimeReadFixture
      (.revoke ⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩)
    [allowed 5 revoked "runtime" "nodes" "node" "read",
     allowed 5 revoked "runtime" "agents" "agent" "read"] = [false, false] := by native_decide

theorem unlinked_agent_is_not_visible_to_runtime :
    allowed 5 { runtimeReadFixture with
      relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩] }
      "runtime" "agents" "agent" "read" = false := by native_decide

end Identity.Configuration
