import Proofs.Store.Identity.Household

namespace Identity.AgentMemoryAccess

open Identity.Household

/-- A Node owned by `user` with an administrator `admin`, and three Agents the
user created: `worker-agent` executed by `worker`, `peer-agent` executed by
`peer` and `other-agent` executed by `other`. `peer` holds `memory_reader` and
`auditor` holds the configuration `reader` on `worker-agent`. `worker` recorded
the experience row `problem` and the fact `note`, both filed under its own
Agent. -/
def fixture : State :=
  let base : State :=
    { objects := [⟨"nodes", "node", "user"⟩, ⟨"agent_memory", "problem", "worker"⟩,
        ⟨"agent_memory_facts", "note", "worker"⟩]
    , relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩,
        ⟨"agents", "worker-agent", "memory_reader", .actor "peer"⟩,
        ⟨"agents", "worker-agent", "reader", .actor "auditor"⟩,
        ⟨"agent_memory", "problem", "parent", .object "agents" "worker-agent"⟩,
        ⟨"agent_memory_facts", "note", "parent", .object "agents" "worker-agent"⟩]
    , enrolled := [] }
  let agents := [("worker-agent", "worker"), ("peer-agent", "peer"), ("other-agent", "other")]
  agents.foldl (fun state (agentId, executor) =>
    commitAgentCreation state agentId "user" executor "node" "runtime" true) base

def rights (state : State) (resource actor id : String) : Bool × Bool × Bool :=
  (allowed 8 state actor resource id "read",
   allowed 8 state actor resource id "update",
   allowed 8 state actor resource id "delete")

/-- Read, update and delete on an experience row. -/
def access (state : State) (actor id : String) : Bool × Bool × Bool :=
  rights state "agent_memory" actor id

/-- Read, update and delete on a fact. -/
def factAccess (state : State) (actor id : String) : Bool × Bool × Bool :=
  rights state "agent_memory_facts" actor id

/-- Whether `actor` may file an experience row and a fact under `agentId`. -/
def files (state : State) (actor agentId : String) : Bool × Bool :=
  (gatedCreateAllowed policy state actor "agent_memory" "agents" agentId,
   gatedCreateAllowed policy state actor "agent_memory_facts" "agents" agentId)

def memoryReader (agentId : String) (subject : DID) : Relation :=
  ⟨"agents", agentId, "memory_reader", .actor subject⟩

def memoryFactReader (agentId : String) (subject : DID) : Relation :=
  ⟨"agents", agentId, "memory_fact_reader", .actor subject⟩

/-- The Agent reads, updates and deletes the experience and the facts it
recorded, and files both under its own Agent. -/
theorem the_agent_controls_its_memory :
    access fixture "worker" "problem" = (true, true, true) ∧
      factAccess fixture "worker" "note" = (true, true, true) ∧
      files fixture "worker" "worker-agent" = (true, true) := by
  native_decide

/-- The user who owns the Node and a Node administrator control the Agent's
memory through `session_admin`, and read its experience through `memory_read`
and its facts through `memory_fact_read`. -/
theorem node_administration_controls_memory :
    ["user", "admin"].map (fun actor => (access fixture actor "problem",
      factAccess fixture actor "note")) =
      [((true, true, true), (true, true, true)), ((true, true, true), (true, true, true))] := by
  native_decide

/-- A `memory_reader` reads the Agent's experience and never writes it; so does
a configuration `reader`. Neither reads its facts. -/
theorem a_memory_reader_reads_experience_but_not_facts :
    ["peer", "auditor"].map (fun actor => (access fixture actor "problem",
      factAccess fixture actor "note")) =
      [((true, false, false), (false, false, false)),
        ((true, false, false), (false, false, false))] := by
  native_decide

/-- An Agent with no grant on the memory's Agent, even one the same user
created on the same Node, neither reads nor writes it. -/
theorem an_unrelated_agent_has_no_access :
    ["other", "stranger"].map (fun actor => (access fixture actor "problem",
      factAccess fixture actor "note")) =
      [((false, false, false), (false, false, false)),
        ((false, false, false), (false, false, false))] := by
  native_decide

/-- Decision (operator, H2): memory is filed under an Agent only by the Agent
itself or by whoever holds its `session_admin`, here the Node's owner and
administrator. Another Agent of the same Node, a `memory_reader`, a
configuration `reader` and a stranger file neither experience nor facts under
it. -/
theorem only_the_agent_and_its_administrators_file_memory :
    ["worker", "user", "admin", "other", "peer", "auditor", "stranger"].map
      (files fixture · "worker-agent") =
      [(true, true), (true, true), (true, true), (false, false), (false, false),
        (false, false), (false, false)] := by
  native_decide

/-- The gate holds for a parent link as for a creation: an Agent links a row
it owns to its own Agent and never to another's, so a row it created elsewhere
is not filed under another Agent afterwards. -/
theorem an_agent_links_its_rows_only_to_its_own_agent :
    ["agent_memory", "agent_memory_facts"].map (fun resource =>
      let forged := directCreate fixture "other" resource "forged" true true
      (grantAdmitted forged "other" ⟨resource, "forged", "parent", .object "agents" "worker-agent"⟩,
       grantAdmitted forged "other" ⟨resource, "forged", "parent", .object "agents" "other-agent"⟩)) =
      [(false, true), (false, true)] := by
  native_decide

/-- One `memory_reader` relationship on the Agent shares every experience row,
including rows recorded after the grant; no relationship per row is needed. -/
theorem one_grant_shares_every_row :
    let recorded := apply (directCreate fixture "worker" "agent_memory" "later" true true)
      (.grantAs "worker" ⟨"agent_memory", "later", "parent", .object "agents" "worker-agent"⟩)
    access recorded "worker" "later" = (true, true, true) ∧
      access recorded "peer" "later" = (true, false, false) ∧
      access recorded "other" "later" = (false, false, false) := by
  native_decide

/-- An administrator of the Agent grants `memory_reader`, which shares its
experience and not its facts. -/
theorem administrator_grants_memory_reader :
    grantAdmitted fixture "admin" (memoryReader "worker-agent" "other") = true ∧
      (let granted := apply fixture (.grantAs "admin" (memoryReader "worker-agent" "other"))
       (access granted "other" "problem", factAccess granted "other" "note")) =
        ((true, false, false), (false, false, false)) := by
  native_decide

/-- The Agent itself, as its executor, shares its own experience. -/
theorem the_agent_shares_its_own_memory :
    grantAdmitted fixture "worker" (memoryReader "worker-agent" "other") = true ∧
      access (apply fixture (.grantAs "worker" (memoryReader "worker-agent" "other")))
        "other" "problem" = (true, false, false) := by
  native_decide

/-- An executor shares only its own Agent's experience, and manages no other
relation, `memory_fact_reader` included; a `memory_reader` cannot pass its
read on. -/
theorem an_executor_shares_only_its_own_memory :
    mayManage fixture "other" (memoryReader "worker-agent" "other") = false ∧
      mayManage fixture "peer" (memoryReader "worker-agent" "other") = false ∧
      ["parent", "reader", "workspace_selected_by", "session_writer", "administrator", "executor",
        "config_writer", "host", "memory_fact_reader"].all (fun relation =>
          !mayManage fixture "worker" ⟨"agents", "worker-agent", relation, .actor "other"⟩) = true := by
  native_decide

/-- Decision (operator, M8): only an administrator shares an Agent's facts.
Neither the Agent itself, as its executor, nor a `memory_reader` grants
`memory_fact_reader`; an administrator's grant lets its subject read the facts
and never write them. -/
theorem only_an_administrator_shares_facts :
    grantAdmitted fixture "worker" (memoryFactReader "worker-agent" "peer") = false ∧
      grantAdmitted fixture "peer" (memoryFactReader "worker-agent" "peer") = false ∧
      grantAdmitted fixture "admin" (memoryFactReader "worker-agent" "peer") = true ∧
      factAccess (apply fixture (.grantAs "admin" (memoryFactReader "worker-agent" "peer")))
        "peer" "note" = (true, false, false) := by
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

/-- `memory_fact_reader` reaches nothing but `memory_fact_read`, which only
facts inherit. -/
theorem memory_fact_reader_grants_only_memory_fact_read :
    (policy.all fun resource => resource.permissions.all fun permission =>
      !permission.relations.contains "memory_fact_reader" ||
        (resource.name == "agents" && permission.name == "memory_fact_read")) = true ∧
    (policy.all fun resource => resource.permissions.all fun permission =>
      !permission.parents.contains ("parent", "memory_fact_read") ||
        resource.name == "agent_memory_facts") = true ∧
    (policy.all fun resource => resource.relations.all fun relation =>
      relation.types.all fun type => !type.endsWith "->memory_fact_reader") = true := by
  native_decide

/-- A permission that names no relation and inherits only `inherited` through
`parent` is held by the document's native owner, or through `inherited` on an
object the document names as parent. -/
theorem allowedIn_through_parent {definitions : List ResourceDefinition} {fuel : Nat}
    {state : State} {actor resource id permission inherited : String}
    (hdefinition : ((definitions.find? (·.name == resource)).bind
      fun definition => definition.permissions.find? (·.name == permission)).map
        (fun definition => (definition.relations, definition.parents)) =
          some ([], [("parent", inherited)]))
    (h : allowedIn definitions (fuel + 1) state actor resource id permission = true) :
    (∃ object ∈ state.objects, object.resource = resource ∧ object.id = id ∧
        object.owner = actor) ∨
      ∃ parentResource parentId,
        (⟨resource, id, "parent", .object parentResource parentId⟩ : Relation) ∈ state.relations ∧
          allowedIn definitions fuel state actor parentResource parentId inherited = true := by
  simp only [allowedIn] at h
  split at h
  · rename_i object definition hobject hpermission
    rw [hpermission] at hdefinition
    simp only [Option.map_some', Option.some.injEq, Prod.mk.injEq] at hdefinition
    obtain ⟨hrelations, hparents⟩ := hdefinition
    rw [hrelations, hparents] at h
    have hfound := List.find?_some hobject
    simp only [Bool.and_eq_true, beq_iff_eq] at hfound
    simp only [List.any_nil, Bool.or_false, List.any_cons, Bool.or_eq_true, beq_iff_eq,
      List.any_eq_true, Bool.and_eq_true] at h
    rcases h with howner | ⟨fact, hmember, ⟨⟨hresource, hid⟩, hrelation⟩, hsubject⟩
    · exact Or.inl ⟨object, List.mem_of_find?_eq_some hobject, hfound.1, hfound.2, howner⟩
    · right
      obtain ⟨factResource, factId, factRelation, subject⟩ := fact
      cases subject with
      | object parentResource parentId =>
        simp only at hresource hid hrelation hsubject
        subst hresource hid hrelation
        exact ⟨parentResource, parentId, hmember, hsubject⟩
      | _ => simp at hsubject
  · rename_i object hobject hnone
    rw [hnone] at hdefinition
    simp at hdefinition
  · simp at h

/-- Document ACP bounds the memory tool's node-wide `delete-document`
(`Configuration.memory_grants_only_delete_document`): a memory row, experience
or fact, is deleted only by its native owner or by whoever holds
`session_admin` on an object the row names as parent, which is an Agent
(`memory_parent_is_an_agent`). -/
theorem memory_delete_is_owner_or_session_admin (fuel : Nat) (state : State)
    (actor resource id : String)
    (hresource : resource = "agent_memory" ∨ resource = "agent_memory_facts")
    (h : allowed (fuel + 1) state actor resource id "delete" = true) :
    (∃ object ∈ state.objects, object.resource = resource ∧ object.id = id ∧
        object.owner = actor) ∨
      ∃ parentResource parentId,
        (⟨resource, id, "parent", .object parentResource parentId⟩ : Relation) ∈ state.relations ∧
          allowed fuel state actor parentResource parentId "session_admin" = true := by
  refine allowedIn_through_parent ?_ h
  rcases hresource with rfl | rfl <;> native_decide

/-- A memory row's parent relationship names an Agent: the relation's type
admits no other subject. -/
theorem memory_parent_is_an_agent (resource id parentResource parentId : String)
    (hresource : resource = "agent_memory" ∨ resource = "agent_memory_facts")
    (h : subjectAdmitted ⟨resource, id, "parent", .object parentResource parentId⟩ = true) :
    parentResource = "agents" := by
  have htypes : ((policy.find? (·.name == resource)).bind
      fun definition => definition.relations.find? (·.name == "parent")).map (·.types) =
        some ["agents"] := by
    rcases hresource with rfl | rfl <;> native_decide
  simp only [subjectAdmitted, subjectAdmittedIn, typeAdmitted, subjectType, Bool.or_false] at h
  split at h
  · rename_i definition hdefinition
    rw [hdefinition] at htypes
    simp only [Option.map_some', Option.some.injEq] at htypes
    simp only [htypes, List.isEmpty_cons, Bool.false_or, List.contains_cons, List.contains_nil,
      Bool.or_false, beq_iff_eq] at h
    exact h
  · simp at h

end Identity.AgentMemoryAccess
