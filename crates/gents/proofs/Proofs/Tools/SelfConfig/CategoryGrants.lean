import Proofs.Store.Identity.Household
import Proofs.Store.Identity.GrantSets
import Proofs.Tools.SelfConfig.Scope

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
`conferredBy` to an Agent today. Since the Engineer is a Node administrator
(`engineerInitRelations`), every grouping is issued to it
(`category_gaps`). An Agent that is not a Node administrator reaches a category
only through the narrower conferrals named here, and where none of those is
issued to it, only through the `self_config_categories` field of its Tools
document, which is not ACP (`narrow_category_gaps`). -/
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
  (`agent_manager_is_catalog_only`). Decision (maintainer, 2026-11-13):
  `agent_manager` takes only the userset of the Node's administrators
  (`Identity.Household.agent_manager_admits_no_actor`), so no actor holds the
  catalog and nothing else: an actor is given the catalog by being made a Node
  administrator, as the Engineer is (`engineerInitRelations`). Choosing the default writes the
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
  `parent->update`. The Engineer holds it on every Agent of its Node, as Node
  administrator; another Agent holds it only as owner of the Agents it created.
* `tools`, `profile`, own: `configuration update` through the `writer` userset
  `agents#config_writer`, issued on the selected Tools and on the selected
  InferenceProfile chain (`grant_selected_inference`). The `profile` category
  also names CompactionConfig, whose `writer` is not issued.
* `backend`, `mcp_service`, `automation`, own: `configuration update` through
  `writer`, which no grant path issues (`grant_selected_inference` issues only
  `selection_manager` on the selected backend), or through `parent->update`,
  which the Engineer holds.
* Every category other than `agent`, foreign: `configuration update` through
  `parent->update`, which is Node administration (`nodes update`), not the
  catalog. The Engineer holds it; no other Agent does unless a User makes it a
  Node administrator.
* `policy` is a node (NAC) grant, not document ACP: `policyCategoryPermissions`. -/
def categoryGroupings : List CategoryGrouping :=
  [ ⟨"node", .foreign, "nodes", "manage_agents",
      [.owner, .relation "administrator", .relation "agent_manager"], true⟩
  , ⟨"agent", .own, "agents", "update", [.relation "config_writer"], true⟩
  , ⟨"agent", .foreign, "agents", "update",
      [.owner, .relation "administrator", .parent "parent" "update"], true⟩
  , ⟨"tools", .own, "configuration", "update", [.relation "writer"], true⟩
  , ⟨"tools", .foreign, "configuration", "update", [.parent "parent" "update"], true⟩
  , ⟨"profile", .own, "configuration", "update", [.relation "writer"], true⟩
  , ⟨"profile", .foreign, "configuration", "update", [.parent "parent" "update"], true⟩
  , ⟨"backend", .own, "configuration", "update",
      [.relation "writer", .parent "parent" "update"], true⟩
  , ⟨"backend", .foreign, "configuration", "update", [.parent "parent" "update"], true⟩
  , ⟨"mcp_service", .own, "configuration", "update",
      [.relation "writer", .parent "parent" "update"], true⟩
  , ⟨"mcp_service", .foreign, "configuration", "update", [.parent "parent" "update"], true⟩
  , ⟨"automation", .own, "configuration", "update",
      [.relation "writer", .parent "parent" "update"], true⟩
  , ⟨"automation", .foreign, "configuration", "update", [.parent "parent" "update"], true⟩ ]

/-- A conferral that needs Node administration: holding it means holding
`nodes update`, or `administrator` on the document itself. -/
def Conferral.needsAdministration : Conferral → Bool
  | .relation "administrator" => true
  | .parent _ "update" => true
  | _ => false

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

/-- No gap remains for the Engineer: every category and scope is issued to an
Agent by ACP. A Rust or policy change that stops issuing one must update the
table. -/
theorem category_gaps :
    (categoryGroupings.filter (!·.issuedToAgents)).map (fun g => (g.category, g.scope)) = [] := by
  decide

/-- What stays out of reach of an Agent that is not a Node administrator: the
groupings every conferral of which needs Node administration or document
ownership, and the three own-scope groupings whose only narrow conferral,
`writer`, no grant path issues. Such an Agent reaches these only through the
`self_config_categories` field of its Tools document, which is not ACP, and its
write is then refused by ACP. -/
theorem narrow_category_gaps :
    (categoryGroupings.filter fun g =>
        g.conferredBy.all (fun c => c.needsAdministration || c == .owner)
          || g.conferredBy.contains (.relation "writer") && g.category ∈
              ["backend", "mcp_service", "automation"]).map (fun g => (g.category, g.scope)) =
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

/-- The grants node initialisation gives the Engineer: `administrator` on the
Node, and `config_writer` on its own Agent document.

Decision (maintainer, 2026-11-11, #3656): the Engineer can do anything it is
asked ("a sharp knife that we give users if they want it"), so it holds full
Node administration in ACP terms rather than the narrow catalog relation.
`administrator` carries `manage_agents` (the catalog), `nodes update`, and
through `parent` the update of every Agent and configuration document of the
Node, which is every self-config category at every scope
(`initialised_engineer_administers_node`).

Accepted cost: `administrator` also gives what configuration does not need:
Node delete, delete and invoke of every Agent, reading and administering every
Agent's sessions, and granting or revoking Node relations, `administrator`
itself included (`administratorPermissions`). There is no narrower grant of the
catalog (`catalog_has_no_direct_holder`).

The two lockout rules bind the Engineer like any actor
(`SelfConfig.lockoutChecks`), a clone of it inherits no grant, and creating an
Agent issues none (`initialised_engineer_holds_catalog`,
`creation_issues_no_catalog`).

Rust work this implies: `gents init` writes `nodes#administrator` for the
Engineer next to `grant_selected_self_configuration`, and agent admission reads
the catalog as a native `manage_agents` check. -/
def engineerInitRelations (engineer node : String) : List Relation :=
  [⟨"nodes", node, "administrator", .actor engineer⟩,
   ⟨"agents", engineer, "config_writer", .actor engineer⟩]

/-- Provisioning the Engineer of a new node: the one step every path that makes
a node's Engineer takes (`gents init`, the desktop's setup through it, the eval
runner's trial node and the fixtures that stand in for an initialised node), so
a change to what the Engineer holds reaches all of them. It adds
`engineerInitRelations` to the node's ACP state and nothing else; the node
permission sets the Engineer holds as the default Agent are issued by the
bootstrap (`GrantSets.receives .defaultAgent`), and the default selection is
written with the Node. -/
def provisionEngineer (state : State) (engineer node : String) : State :=
  { state with relations := engineerInitRelations engineer node ++ state.relations }

/-- A Node with one Agent `a`, a configuration document and a session under it;
`admin` is its administrator, the Node's administrators are its
`agent_manager` userset, `runtime` holds `runtime_reader`. Init has created the
default selection. -/
def catalogRelationFixture : State :=
  let base : State :=
    { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "cfg", "user"⟩,
        ⟨"sessions", "s", "a"⟩]
    , relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩,
        ⟨"nodes", "node", "agent_manager", .entitySet "nodes" "node" "administrator"⟩,
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
catalog (`nodes manage_agents`: owner or administrator) may
update the selection, and nobody else may; whoever reads the Node or holds the
catalog reads it.

Premise: the lockout rule (`SelfConfig.lockoutChecks`) reads the current
default authoritatively, as the runtime reads the Node, not through the acting
actor's read grants; an actor that may update an Agent but cannot read the
selection must still be refused disabling the default. -/
theorem default_selection_write_is_the_catalog :
    ["user", "admin", "runtime", "a", "stranger"].map (fun actor =>
      (allowed 8 catalogRelationFixture actor "nodes" "node" "manage_agents",
       defaultSelectionWrite catalogRelationFixture actor "node",
       allowed 8 catalogRelationFixture actor "default_agent_selections" "node" "read",
       allowed 8 catalogRelationFixture actor "nodes" "node" "read")) =
      [(true, true, true, true), (true, true, true, true),
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

/-- The catalog has no direct holder: an administrator, who manages
`agent_manager`, is still refused adding an actor or the wildcard to it, and
is admitted adding the Node's administrators. The owner and the administrator
hold the catalog and everything `administrator` gives; an Agent with no Node
relation and the runtime reader hold neither. -/
theorem catalog_has_no_direct_holder :
    mayManage catalogRelationFixture "admin" ⟨"nodes", "node", "agent_manager", .actor "x"⟩
      = true ∧
    grantAdmitted catalogRelationFixture "admin" ⟨"nodes", "node", "agent_manager", .actor "x"⟩
      = false ∧
    grantAdmitted catalogRelationFixture "admin" ⟨"nodes", "node", "agent_manager", .wildcard⟩
      = false ∧
    grantAdmitted catalogRelationFixture "admin"
      ⟨"nodes", "node", "agent_manager", .entitySet "nodes" "node" "administrator"⟩ = true ∧
    grantAdmitted catalogRelationFixture "admin" ⟨"nodes", "node", "administrator", .actor "x"⟩
      = true ∧
    ["user", "admin"].all (fun actor =>
      allowed 8 catalogRelationFixture actor "nodes" "node" "manage_agents" &&
        administratorPermissions.all (fun (res, id, perm) =>
          allowed 8 catalogRelationFixture actor res id perm)) = true ∧
    ["runtime", "a", "stranger"].all (fun actor =>
      !allowed 8 catalogRelationFixture actor "nodes" "node" "manage_agents") = true := by
  native_decide

/-- `catalogRelationFixture` after init publishes the Engineer and its grants. -/
def initialisedEngineerState : State :=
  provisionEngineer
    (commitAgentCreation catalogRelationFixture "engineer" "user" "engineer" "node" true)
    "engineer" "node"

/-- The Engineer as initialised holds the catalog and configures every Agent
of its Node, the User's included; a clone it then makes holds no category,
because creation copies neither relation. -/
theorem initialised_engineer_holds_catalog :
    actorGrantsOf initialisedEngineerState "engineer" "node" ["engineer", "a"] =
      ⟨true, ["engineer", "a"]⟩ ∧
    actorGrantsOf
      (commitAgentCreation initialisedEngineerState "clone" "engineer" "clone" "node" true)
      "clone" "node" ["clone", "engineer", "a"] = ⟨false, []⟩ := by
  native_decide

/-- By the policy itself the initialised Engineer holds every permission a Node
administrator holds, on the Node, on a User's Agent, on a configuration
document and on a session under that Agent: every self-config category at
every scope, and the accepted extras named on `engineerInitRelations`. -/
theorem initialised_engineer_administers_node :
    administratorPermissions.all (fun (res, id, perm) =>
      allowed 8 initialisedEngineerState "engineer" res id perm) = true ∧
    allowed 8 initialisedEngineerState "engineer" "nodes" "node" "manage_agents" = true ∧
    defaultSelectionWrite initialisedEngineerState "engineer" "node" = true := by
  native_decide

/-- The Engineer's catalog is its Node administration and nothing else:
withdrawing `nodes#administrator` from it removes creating and cloning Agents
and choosing the default, and no `agent_manager` relationship restores them. -/
theorem engineer_without_administration_loses_catalog :
    let revoked := apply initialisedEngineerState
      (.revokeAs "user" ⟨"nodes", "node", "administrator", .actor "engineer"⟩)
    allowed 8 revoked "engineer" "nodes" "node" "manage_agents" = false ∧
    defaultSelectionWrite revoked "engineer" "node" = false ∧
    allowed 8 revoked "admin" "nodes" "node" "manage_agents" = true ∧
    grantAdmitted revoked "user" ⟨"nodes", "node", "agent_manager", .actor "engineer"⟩ = false := by
  native_decide

/-- Creating or cloning an Agent never issues the catalog relation. -/
theorem creation_issues_no_catalog (child childDid node : String) :
    (creationRelations child childDid node).all (·.relation != "agent_manager") = true := by
  simp [creationRelations]

end SelfConfig
