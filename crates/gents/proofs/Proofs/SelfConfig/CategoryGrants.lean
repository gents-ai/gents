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

* `node` (the agent catalog) is `nodes update` on the Node: its owner (the init
  User) or a `nodes#administrator` (an enrolled member,
  `Identity.EnrollmentGrant`). It confers `agents update` on every Agent
  through the `administrator` usersets `commitAgentCreation` publishes and
  through `parent->update`, `configuration update` through `parent->update`,
  and the Node's `default_agent_did`. Gap: no Agent holds it today; the
  Engineer's catalog exists only as its Tools field. The premise for closing
  it is that the Engineer (or any catalog Agent) is granted
  `nodes#administrator`, or that the policy gains a narrower Node relation
  conferring `agents update` and Node update without `nodes#administrator`'s
  inherited session access (`agents.session_read` through `parent->delete`).
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
  `parent->update`, which is the catalog's Node update.
* `policy` is a node (NAC) grant, not document ACP: `policyCategoryPermissions`. -/
def categoryGroupings : List CategoryGrouping :=
  [ ⟨"node", .foreign, "nodes", "update", [.owner, .relation "administrator"], false⟩
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
      [("node", .foreign), ("agent", .foreign), ("tools", .foreign), ("profile", .foreign),
       ("backend", .own), ("backend", .foreign), ("mcp_service", .own),
       ("mcp_service", .foreign), ("automation", .own), ("automation", .foreign)] := by
  decide

/-! ## The grants an agent operation reads -/

/-- What the acting DID holds for agent operations, read from native ACP:
`catalog` is `nodes update` on the Node, and `configures` lists the Agents whose
document it may update. A person and an Agent are read the same way. -/
structure ActorGrants where
  catalog : Bool
  configures : List String
  deriving DecidableEq, Repr

/-- Fuel matches `Identity.Household.mayManage`; the Agent → Node chain needs
three levels. The model names each Agent document by its Agent DID. -/
def actorGrantsOf (s : State) (actor node : String) (agents : List String) : ActorGrants :=
  { catalog := allowed 8 s actor "nodes" node "update"
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

end SelfConfig
