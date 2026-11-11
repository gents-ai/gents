import Proofs.Identity.Household
import Proofs.Identity.GrantSets
import Proofs.SelfConfig.Scope

/-!
# Self-config categories as groupings of ACP grants

A self-config category is a name for a set of native ACP grants, not a second
authorization layer beside the database. An actor, person or Agent, holds a
category exactly when DefraDB's document ACP (`Identity.Household.policy`)
grants it the permission the category groups, on the document it acts on.
Admission of agent operations (`SelfConfig.agentAdmission`) is stated over
these grants.

Rust owners: the grant writers `grant_selected_self_configuration`
(`store/config_client/desired_state.rs`) and `set_agent_self_config_permission`
(`store/config_client/self_config_permission.rs`), and the generated policy
`crates/gents-schemas/schemas/agent/agent_ownership_policy.yaml`.
-/

namespace SelfConfig

open Identity.Household

/-- How a policy permission is conferred: by native ownership of the document,
by a relation named in the permission's expression, or through a parent
relation (`parent->update`). -/
inductive Conferral where
  | owner
  | relation (name : String)
  | parent (relation inherited : String)
  deriving DecidableEq, Repr

/-- One category at one scope, as the ACP permission it groups.

`issuedToAgents` records whether any Gents grant path issues one of
`conferredBy` to an Agent today. When it is `false` the category reaches an
Agent only through the `self_config_categories` field of its Tools document,
which is not ACP: that is a gap the policy and the Rust grant writers must
close before the field can be retired. -/
structure CategoryGrouping where
  category : String
  scope : Scope
  resource : String
  permission : String
  conferredBy : List Conferral
  issuedToAgents : Bool
  deriving DecidableEq, Repr

/-- The intended correspondence between every self-config category and the
ACP grants of `Identity.Household.policy`.

Decision (maintainer, 2026-11-11, #3656): categories are application-useful
groupings of ACP grants, and a server must support several agents and several
users with different limits, so every actor passes the same category rule and
the rule is evaluated by ACP.

* `node` (the agent catalog: create and clone an Agent, choose the default
  Agent) is `nodes manage_agents` on the Node: the narrow relation
  `agent_manager`, or the stronger owner (the init User) and `administrator`
  (an enrolled member, `Identity.EnrollmentGrant`). Decision (maintainer,
  2026-11-11, #3656): a new relation rather than `nodes#administrator`, so the
  catalog carries none of `administrator`'s Node update and delete, and none of
  the Agent, configuration and session permissions `parent` derives from them
  (`agent_manager_is_catalog_only`). The Engineer receives it at node
  initialisation (`engineerInitRelations`). Choosing the default writes the
  Node's `DefaultAgentSelection` document, whose update is its parent Node's
  `manage_agents` (`default_selection_write_is_the_catalog`), so document ACP
  authorizes it directly. The catalog does not reach another Agent's
  documents: editing one is the agent category on it.
* `agent`, own: `agents update` through `config_writer` on the actor's own
  Agent document, and `configuration update` through the `writer` userset
  `agents#config_writer` on its selected Context. Both are issued by
  `grant_selected_self_configuration` and `gents config agent
  grant-self-config`.
* `agent`, foreign: `agents update` on the target: its owner (the creator of an
  Agent-created Agent), a direct or Node-derived `administrator`, or
  `parent->update`. Agents hold it only as owners of the Agents they created.
* `tools`, `profile`, own: `configuration update` through the `writer` userset
  `agents#config_writer`, issued on the selected Tools and on the selected
  InferenceProfile chain (`grant_selected_inference`). The `profile` category
  also names CompactionConfig, whose `writer` is not issued.
* `backend`, `mcp_service`, `automation`, own: `configuration update` through
  `writer`. Gap: no grant path issues it; `grant_selected_inference` issues only
  `selection_manager` on the selected backend.
* Every category other than `agent`, foreign: `configuration update` through
  `parent->update`, which is Node administration (`nodes update`), not the
  catalog. The tool's `requiredCategories` still asks for `node` plus the
  category there; in ACP terms that is a gap for every Agent.
* `policy` is a node (NAC) grant, not document ACP: `policyCategoryPermissions`. -/
def categoryGroupings : List CategoryGrouping :=
  [ ⟨"node", .foreign, "nodes", "manage_agents",
      [.owner, .relation "administrator", .relation "agent_manager"], true⟩
  , ⟨"agent", .own, "agents", "update", [.relation "config_writer"], true⟩
  , ⟨"agent", .foreign, "agents", "update",
      [.owner, .relation "administrator", .parent "parent" "update"], false⟩
  , ⟨"tools", .own, "configuration", "update", [.relation "writer"], true⟩
  , ⟨"tools", .foreign, "configuration", "update", [.parent "parent" "update"], false⟩
  , ⟨"profile", .own, "configuration", "update", [.relation "writer"], true⟩
  , ⟨"profile", .foreign, "configuration", "update", [.parent "parent" "update"], false⟩
  , ⟨"backend", .own, "configuration", "update", [.relation "writer"], false⟩
  , ⟨"backend", .foreign, "configuration", "update", [.parent "parent" "update"], false⟩
  , ⟨"mcp_service", .own, "configuration", "update", [.relation "writer"], false⟩
  , ⟨"mcp_service", .foreign, "configuration", "update", [.parent "parent" "update"], false⟩
  , ⟨"automation", .own, "configuration", "update", [.relation "writer"], false⟩
  , ⟨"automation", .foreign, "configuration", "update", [.parent "parent" "update"], false⟩ ]

/-- The `policy` category groups the node delegation set the default Agent
receives directly (`Identity.GrantSets.directSets`); the policy tool admits a
change exactly when native NAC does (`Identity.GrantSets.policyToolAdmits`). -/
def policyCategoryPermissions : List String :=
  Identity.GrantSets.GrantSet.policyDelegate.permissions

def Conferral.inPolicy (resource permission : String) : Conferral → Bool
  | .owner => true
  | .relation name =>
      ((policy.find? (·.name == resource)).bind
        (·.permissions.find? (·.name == permission))).any (·.relations.contains name)
  | .parent via inherited =>
      ((policy.find? (·.name == resource)).bind
        (·.permissions.find? (·.name == permission))).any
        (·.parents.contains (via, inherited))

/-- Every grouping names a permission of the generated policy, conferred by
relations its expression actually contains: the correspondence cannot name a
grant DefraDB does not evaluate. -/
theorem groupings_name_policy_grants :
    categoryGroupings.all (fun g => g.conferredBy.all (·.inPolicy g.resource g.permission))
      = true := by
  native_decide

/-- Every category is grouped, and `policy` is the only one outside document
ACP. -/
theorem every_category_is_grouped :
    selfConfigCategories.all (fun c =>
      c == "policy" || categoryGroupings.any (·.category == c)) = true := by
  decide

/-- The gaps, exactly: the categories whose grant reaches an Agent only through
its Tools field today. A Rust or policy change that issues one of them must
update this list. -/
theorem category_gaps :
    (categoryGroupings.filter (!·.issuedToAgents)).map (fun g => (g.category, g.scope)) =
      [("agent", .foreign), ("tools", .foreign), ("profile", .foreign),
       ("backend", .own), ("backend", .foreign), ("mcp_service", .own),
       ("mcp_service", .foreign), ("automation", .own), ("automation", .foreign)] := by
  decide

/-! ## The grants an agent operation reads -/

/-- What the acting DID holds for agent operations, read from native ACP:
`catalog` is `nodes manage_agents` on the Node, and `configures` lists the Agents whose
document it may update. A person and an Agent are read the same way. -/
structure ActorGrants where
  catalog : Bool
  configures : List String
  deriving DecidableEq, Repr

/-- Fuel matches `Identity.Household.mayManage`; the Agent → Node chain needs
three levels. The model names each Agent document by its Agent DID. -/
def actorGrantsOf (s : State) (actor node : String) (agents : List String) : ActorGrants :=
  { catalog := allowed 8 s actor "nodes" node "manage_agents"
  , configures := agents.filter fun a => allowed 8 s actor "agents" a "update" }

/-- The relations creating or cloning an Agent publishes
(`Identity.Household.commitAgentCreation`). -/
def creationRelations (child childDid node : String) : List Relation :=
  [⟨"agents", child, "parent", .object "nodes" node⟩,
   ⟨"agents", child, "executor", .actor childDid⟩,
   ⟨"agents", child, "administrator", .entitySet "nodes" node "owner"⟩,
   ⟨"agents", child, "administrator", .entitySet "nodes" node "administrator"⟩]

theorem creation_publishes_creation_relations (s : State) (child creator childDid node : String) :
    (commitAgentCreation s child creator childDid node true).relations =
      creationRelations child childDid node ++ s.relations := rfl

/-- A clone, like any created Agent, receives no self-config grant: creation
publishes relations on the new Agent document only, none of them
`config_writer`, and names the new Agent's DID only as `executor`, which no
update permission contains. Self-configuration stays an explicit later grant
(`clone_inherits_no_self_config` states the same of its Tools). -/
theorem creation_grants_no_category (child childDid node : String) :
    (creationRelations child childDid node).map (fun r => (r.resource, r.relation)) =
      [("agents", "parent"), ("agents", "executor"), ("agents", "administrator"),
       ("agents", "administrator")] ∧
    (Conferral.relation "executor").inPolicy "agents" "update" = false ∧
    (Conferral.relation "executor").inPolicy "nodes" "update" = false := by
  refine ⟨rfl, ?_, ?_⟩ <;> native_decide

/-! ## The catalog relation -/

/-- `agent_manager` is named only by `nodes manage_agents`; `manage_agents` is
inherited only by `default_agent_selections` read and update; no resource names
`default_agent_selections` as a parent or userset type; and no relation type
reaches `nodes->agent_manager`. The relation flows into the default selection
write and nothing else. -/
theorem agent_manager_is_catalog_only :
    (policy.flatMap fun r => r.permissions.filterMap fun p =>
        if p.relations.contains "agent_manager" then some (r.name, p.name) else none) =
      [("nodes", "manage_agents")] ∧
    (policy.flatMap fun r => r.permissions.filterMap fun p =>
        if p.parents.any (·.2 == "manage_agents") then some (r.name, p.name) else none) =
      [("default_agent_selections", "read"), ("default_agent_selections", "update")] ∧
    policy.all (fun r => r.relations.all (fun rel =>
      rel.types.all (fun t => !(t == "default_agent_selections" ||
        t.startsWith "default_agent_selections->")))) = true ∧
    policy.all (fun r => r.relations.all (fun rel => !rel.types.contains "nodes->agent_manager"))
      = true := by
  native_decide

/-- The Node's default Agent selection document. Its ID is the Node's DID: it
is keyed by `node_did` (`ConfigDocuments.Collection.defaultAgentSelection`), so
a Node has at most one.

Premise: node initialisation creates it, as the init User (the Node's owner,
whose key signs user configuration), in the transaction that publishes the
Node, with its `parent` relation to the Node; nothing else creates or deletes
it while the Node exists. A Node therefore has exactly one, from init on
(`initialised_node_has_one_default_selection`).

Rust and SDL work this implies: a new SDL type `DefaultAgentSelection`
(`node_did`, `default_agent_did`, `updated_at`) bound to the
`default_agent_selections` policy resource; `default_agent_did` removed from the
Node SDL and `Node` struct; `gents init` creating the document as the init
User; every reader of `Node.default_agent_did` (runtime default resolution,
agent catalog and list, desktop, CLI, desired-state apply and publication
validation, node readiness) repointed to it; a migration moving each existing
Node's value into a new selection document; and the policy ID change
(`AGENT_OWNERSHIP_POLICY_ID`, the generated
`agent_ownership_policy.yaml`) with its registration upgrade. -/
def initDefaultSelection (s : State) (node initUser : String) : State :=
  { s with
    objects := ⟨"default_agent_selections", node, initUser⟩ :: s.objects
    relations := ⟨"default_agent_selections", node, "parent", .object "nodes" node⟩ ::
      s.relations }

/-- The permission an actor needs to choose the default: update of the Node's
selection document. -/
def defaultSelectionWrite (s : State) (actor node : String) : Bool :=
  allowed 8 s actor "default_agent_selections" node "update"

/-- The grants node initialisation gives the Engineer: the catalog relation on
the Node, and `config_writer` on its own Agent document.

Rust and policy work this implies: the generated policy
(`crates/gents-schemas/schemas/agent/agent_ownership_policy.yaml`, from
`GenerateHouseholdPolicy`, with `AGENT_OWNERSHIP_POLICY_ID` and the policy
registration upgrade the ID change requires) gains the `agent_manager` relation
and `manage_agents` permission on `nodes`; `gents init` writes
`nodes#agent_manager` for the Engineer next to
`grant_selected_self_configuration`; and agent admission reads the catalog as a
native `manage_agents` check. DefraDB authorizes a document update as a whole,
so writing the Node's `default_agent_did` under `manage_agents` alone also
needs a write path that does not require `nodes update`. -/
def engineerInitRelations (engineer node : String) : List Relation :=
  [⟨"nodes", node, "agent_manager", .actor engineer⟩,
   ⟨"agents", engineer, "config_writer", .actor engineer⟩]

/-- A Node with one Agent `a`, a configuration document and a session under it;
`admin` is its administrator, `manager` holds only `agent_manager`, `runtime`
holds `runtime_reader`. Init has created the default selection. -/
def catalogRelationFixture : State :=
  let base : State :=
    { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "cfg", "user"⟩,
        ⟨"sessions", "s", "a"⟩]
    , relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩,
        ⟨"nodes", "node", "agent_manager", .actor "manager"⟩,
        ⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
        ⟨"configuration", "cfg", "parent", .object "nodes" "node"⟩,
        ⟨"sessions", "s", "parent", .object "agents" "a"⟩]
    , enrolled := [] }
  initDefaultSelection (commitAgentCreation base "a" "user" "a" "node" true) "node" "user"

theorem initialised_node_has_one_default_selection :
    (catalogRelationFixture.objects.filter fun o =>
      o.resource == "default_agent_selections" && o.id == "node").length = 1 := by
  native_decide

/-- Choosing the default is authorized by the policy itself: whoever holds the
catalog (`nodes manage_agents`: owner, administrator, `agent_manager`) may
update the selection, and nobody else may; whoever reads the Node or holds the
catalog reads it.

Premise: the lockout rule (`SelfConfig.lockoutChecks`) reads the current
default authoritatively, as the runtime reads the Node, not through the acting
actor's read grants; an actor that may update an Agent but cannot read the
selection must still be refused disabling the default. -/
theorem default_selection_write_is_the_catalog :
    ["user", "admin", "manager", "runtime", "a", "stranger"].map (fun actor =>
      (allowed 8 catalogRelationFixture actor "nodes" "node" "manage_agents",
       defaultSelectionWrite catalogRelationFixture actor "node",
       allowed 8 catalogRelationFixture actor "default_agent_selections" "node" "read",
       allowed 8 catalogRelationFixture actor "nodes" "node" "read")) =
      [(true, true, true, true), (true, true, true, true), (true, true, true, false),
       (false, false, true, true), (false, false, false, false),
       (false, false, false, false)] := by
  native_decide

/-- The permissions `administrator` holds over the Node and everything under it. -/
def administratorPermissions : List (String × String × String) :=
  [("nodes", "node", "read"), ("nodes", "node", "update"), ("nodes", "node", "delete"),
   ("agents", "a", "read"), ("agents", "a", "update"), ("agents", "a", "delete"),
   ("agents", "a", "invoke"), ("agents", "a", "session_read"), ("agents", "a", "session_admin"),
   ("configuration", "cfg", "read"), ("configuration", "cfg", "update"),
   ("configuration", "cfg", "delete"),
   ("sessions", "s", "read"), ("sessions", "s", "update"), ("sessions", "s", "delete")]

/-- `agent_manager` gives the catalog and nothing `administrator` gives beyond
it: no Node read, update or delete, no Agent read, update, delete, invoke or
session access, no configuration or session access, and no relation management. -/
theorem agent_manager_gives_catalog_only :
    allowed 8 catalogRelationFixture "manager" "nodes" "node" "manage_agents" = true ∧
    allowed 8 catalogRelationFixture "admin" "nodes" "node" "manage_agents" = true ∧
    allowed 8 catalogRelationFixture "user" "nodes" "node" "manage_agents" = true ∧
    administratorPermissions.all (fun (res, id, perm) =>
      allowed 8 catalogRelationFixture "admin" res id perm) = true ∧
    administratorPermissions.all (fun (res, id, perm) =>
      !allowed 8 catalogRelationFixture "manager" res id perm) = true ∧
    ["administrator", "runtime_reader", "agent_manager"].all (fun rel =>
      !mayManage catalogRelationFixture "manager" ⟨"nodes", "node", rel, .actor "x"⟩) = true ∧
    mayManage catalogRelationFixture "admin" ⟨"nodes", "node", "agent_manager", .actor "x"⟩
      = true := by
  native_decide

/-- `catalogRelationFixture` after init publishes the Engineer and its grants. -/
def initialisedEngineerState : State :=
  let created := commitAgentCreation catalogRelationFixture "engineer" "user" "engineer" "node" true
  { created with relations := engineerInitRelations "engineer" "node" ++ created.relations }

/-- The Engineer as initialised holds the catalog and configures itself, and
nothing else; a clone it then makes holds no category, because creation copies
neither relation. -/
theorem initialised_engineer_holds_catalog :
    actorGrantsOf initialisedEngineerState "engineer" "node" ["engineer", "a"] =
      ⟨true, ["engineer"]⟩ ∧
    actorGrantsOf
      (commitAgentCreation initialisedEngineerState "clone" "engineer" "clone" "node" true)
      "clone" "node" ["clone", "engineer", "a"] = ⟨false, []⟩ := by
  native_decide

/-- Creating or cloning an Agent never issues the catalog relation. -/
theorem creation_issues_no_catalog (child childDid node : String) :
    (creationRelations child childDid node).all (·.relation != "agent_manager") = true := by
  simp [creationRelations]

end SelfConfig
