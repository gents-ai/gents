import Proofs.Store.Identity.Household

namespace Identity.AgentMemoryAccess

open Identity.Household

/-- A Node owned by `user` with an administrator `admin`, and three Agents the
user created: `worker-agent` executed by `worker`, `peer-agent` executed by
`peer` and `other-agent` executed by `other`. `peer` holds `memory_reader` and
`auditor` holds the configuration `reader` on `worker-agent`. `worker` recorded
the memory row `problem`, parented to its own Agent. -/
def fixture : State :=
  let base : State :=
    { objects := [⟨"nodes", "node", "user"⟩, ⟨"agent_memory", "problem", "worker"⟩]
    , relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩,
        ⟨"agents", "worker-agent", "memory_reader", .actor "peer"⟩,
        ⟨"agents", "worker-agent", "reader", .actor "auditor"⟩,
        ⟨"agent_memory", "problem", "parent", .object "agents" "worker-agent"⟩]
    , enrolled := [] }
  let agents := [("worker-agent", "worker"), ("peer-agent", "peer"), ("other-agent", "other")]
  agents.foldl (fun state (agentId, executor) =>
    commitAgentCreation state agentId "user" executor "node" "runtime" true) base

def access (state : State) (actor id : String) : Bool × Bool × Bool :=
  (allowed 8 state actor "agent_memory" id "read",
   allowed 8 state actor "agent_memory" id "update",
   allowed 8 state actor "agent_memory" id "delete")

def memoryReader (agentId : String) (subject : DID) : Relation :=
  ⟨"agents", agentId, "memory_reader", .actor subject⟩

/-- The Agent reads, updates and deletes the memory it recorded, and creates
memory under its own Agent: the resource has no create gate. -/
theorem the_agent_controls_its_memory :
    access fixture "worker" "problem" = (true, true, true) ∧
    gatedCreateAllowed policy fixture "worker" "agent_memory" "agents" "worker-agent" = true := by
  native_decide

/-- The user who owns the Node and a Node administrator control the Agent's
memory through `session_admin` and read it through `memory_read`. -/
theorem node_administration_controls_memory :
    ["user", "admin"].map (access fixture · "problem") = [(true, true, true), (true, true, true)] := by
  native_decide

/-- A `memory_reader` reads the Agent's memory and never writes it; so does a
configuration `reader`. -/
theorem a_memory_reader_reads_but_never_writes :
    ["peer", "auditor"].map (access fixture · "problem") =
      [(true, false, false), (true, false, false)] := by
  native_decide

/-- An Agent with no grant on the memory's Agent, even one the same user
created on the same Node, neither reads nor writes it. -/
theorem an_unrelated_agent_has_no_access :
    ["other", "stranger"].map (access fixture · "problem") =
      [(false, false, false), (false, false, false)] := by
  native_decide

/-- One `memory_reader` relationship on the Agent shares every row, including
rows recorded after the grant; no relationship per row is needed. -/
theorem one_grant_shares_every_row :
    let recorded := apply (directCreate fixture "worker" "agent_memory" "fact" true true)
      (.grantAs "worker" ⟨"agent_memory", "fact", "parent", .object "agents" "worker-agent"⟩)
    access recorded "worker" "fact" = (true, true, true) ∧
      access recorded "peer" "fact" = (true, false, false) ∧
      access recorded "other" "fact" = (false, false, false) := by
  native_decide

/-- An administrator of the Agent grants `memory_reader`. -/
theorem administrator_grants_memory_reader :
    grantAdmitted fixture "admin" (memoryReader "worker-agent" "other") = true ∧
      access (apply fixture (.grantAs "admin" (memoryReader "worker-agent" "other")))
        "other" "problem" = (true, false, false) := by
  native_decide

/-- The Agent itself, as its executor, shares its own memory. -/
theorem the_agent_shares_its_own_memory :
    grantAdmitted fixture "worker" (memoryReader "worker-agent" "other") = true ∧
      access (apply fixture (.grantAs "worker" (memoryReader "worker-agent" "other")))
        "other" "problem" = (true, false, false) := by
  native_decide

/-- An executor shares only its own Agent's memory, and manages no other
relation; a `memory_reader` cannot pass its read on. -/
theorem an_executor_shares_only_its_own_memory :
    mayManage fixture "other" (memoryReader "worker-agent" "other") = false ∧
      mayManage fixture "peer" (memoryReader "worker-agent" "other") = false ∧
      ["parent", "reader", "workspace_selected_by", "session_writer", "administrator", "executor",
        "config_writer", "host"].all (fun relation =>
          !mayManage fixture "worker" ⟨"agents", "worker-agent", relation, .actor "other"⟩) = true := by
  native_decide

/-- Revoking `memory_reader` withdraws the read, whoever granted it. -/
theorem revoked_memory_reader_stops_reading :
    access (apply fixture (.revokeAs "worker" (memoryReader "worker-agent" "peer")))
      "peer" "problem" = (false, false, false) := by
  native_decide

/-- `memory_reader` reaches nothing but `memory_read`: no other permission
names it and no relation takes it as a userset. -/
theorem memory_reader_grants_only_memory_read :
    (policy.all fun resource => resource.permissions.all fun permission =>
      !permission.relations.contains "memory_reader" ||
        (resource.name == "agents" && permission.name == "memory_read")) = true ∧
    (policy.all fun resource => resource.relations.all fun relation =>
      relation.types.all fun type => !type.endsWith "->memory_reader") = true := by
  native_decide

end Identity.AgentMemoryAccess
