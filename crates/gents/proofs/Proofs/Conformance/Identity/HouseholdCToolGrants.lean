import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family
import Proofs.Store.Identity.GrantSets

namespace Conformance.HouseholdC

open Lean Identity Identity.Household Identity.Conformance Identity.Conformance.ConfigurationFixture

structure SourceWire where
  toolsDoc : String
  agent : String
  purpose : String
  permission : String
  deriving ToJson

def SourceWire.ofModel (source : Configuration.ToolGrantSource) : SourceWire :=
  ⟨source.toolsDoc, source.agent, source.purpose, source.permission⟩

def SourceWire.toModel (source : SourceWire) : Configuration.ToolGrantSource :=
  ⟨source.toolsDoc, source.agent, source.purpose, source.permission⟩

structure DirectWire where
  actor : String
  permission : String
  deriving ToJson

def DirectWire.ofModel (grant : String × String) : DirectWire := ⟨grant.1, grant.2⟩

/-- A projection state as the native relations hold it: the selected sources,
their projected grants, direct grants, owners and administrators. -/
structure ProjectionWire where
  enabled : List SourceWire
  grants : List SourceWire
  direct : List DirectWire
  owners : List String
  admins : List String
  deriving ToJson

def ProjectionWire.ofModel (state : Configuration.ToolGrantProjection) : ProjectionWire :=
  ⟨state.enabled.map .ofModel, state.grants.map .ofModel, state.direct.map .ofModel,
    state.owners, state.admins⟩

def ProjectionWire.toModel (state : ProjectionWire) : Configuration.ToolGrantProjection :=
  { enabled := state.enabled.map (·.toModel), grants := state.grants.map (·.toModel),
    direct := state.direct.map fun grant => (grant.actor, grant.permission),
    owners := state.owners, admins := state.admins }

structure ProjectionInput where
  name : String
  initial : ProjectionWire
  source : SourceWire
  enable : Bool
  authorized : Bool
  committed : Bool
  deriving ToJson

structure ProjectionOutput where
  enabled : List SourceWire
  grants : List SourceWire
  effective : Bool
  deriving ToJson

private def projectionRow (name : String) (initial : Configuration.ToolGrantProjection)
    (source : Configuration.ToolGrantSource) (enable authorized committed : Bool) : ProjectionInput :=
  { name, initial := .ofModel initial, source := .ofModel source, enable, authorized, committed }

private def selectedProjection : Configuration.ToolGrantProjection :=
  { enabled := [toolSource], grants := [toolSource] }

def projectionInputs : List ProjectionInput :=
  [projectionRow "enable" {} toolSource true true true,
   projectionRow "disable" selectedProjection toolSource false true true,
   projectionRow "direct-survives" { selectedProjection with direct := [(agentDid, "patch-collection")] } toolSource false true true,
   projectionRow "admin-survives" { selectedProjection with admins := [agentDid] } toolSource false true true,
   projectionRow "unauthorized-enable" {} toolSource true false true,
   projectionRow "failed-enable" {} toolSource true true false,
   projectionRow "unauthorized-disable" selectedProjection toolSource false false true,
   projectionRow "failed-disable" selectedProjection toolSource false true false,
   projectionRow "missing-physical-document" {} { toolSource with toolsDoc := "" } true true true,
   projectionRow "idempotent-enable" selectedProjection toolSource true true true]

/-- A production Agent selects one Tools document through its one Context,
only the schema purpose projects `patch-collection`, and the Node's native
owner is fixed when the Node is created and is never an Agent, so these initial
states (a second Tools document, a second purpose, a sibling's source for the
same selection, the managed Agent as the Node's owner) have no production form. -/
def multiSourceProjectionInputs : List ProjectionInput :=
  [projectionRow "other-source-survives" { selectedProjection with enabled := [toolSource, otherToolSource], grants := [toolSource, otherToolSource] } toolSource false true true,
   projectionRow "other-purpose-survives" { selectedProjection with enabled := [toolSource, otherPurposeSource], grants := [toolSource, otherPurposeSource] } toolSource false true true,
   projectionRow "other-agent-survives" { selectedProjection with enabled := [toolSource, otherAgentSource], grants := [toolSource, otherAgentSource] } toolSource false true true,
   projectionRow "owner-survives" { selectedProjection with owners := [agentDid] } toolSource false true true]

def projectionOutput (i : ProjectionInput) : ProjectionOutput :=
  let result := Configuration.changeToolGrant i.initial.toModel i.source.toModel i.enable
    i.authorized i.committed
  { enabled := result.enabled.map .ofModel, grants := result.grants.map .ofModel,
    effective := Configuration.toolGrantEffective result i.source.agent i.source.permission }

/-- A per-source disable removes only its own source: every other source,
and the owner's authority, survives it. -/
theorem disable_removes_only_its_source :
    multiSourceProjectionInputs.map (fun i =>
      ((projectionOutput i).enabled.length, (projectionOutput i).effective)) =
      [(1, true), (1, true), (1, false), (0, true)] := by
  native_decide

structure PermissionAllowed where
  permission : String
  allowed : Bool
  deriving ToJson

structure GroupInput where
  name : String
  sources : List SourceWire
  initial : ProjectionWire
  enable : Bool
  authorized : Bool
  committed : Bool
  deriving ToJson

structure GroupOutput where
  enabled : List SourceWire
  grants : List SourceWire
  permissions : List PermissionAllowed
  deriving ToJson

private def groupRow (name : String) (initial : Configuration.ToolGrantProjection)
    (sources : List Configuration.ToolGrantSource) (enable authorized committed : Bool) : GroupInput :=
  { name, sources := sources.map .ofModel, initial := .ofModel initial, enable, authorized,
    committed }

def groupInputs : List GroupInput :=
  [groupRow "schema-enable-all" {} schemaToolSources true true true,
   groupRow "schema-enable-rejected" {} schemaToolSources true false true,
   groupRow "schema-enable-rollback" {} schemaToolSources true true false]

/-- Only the schema purpose projects `patch-collection`, so an initial state
that also holds an `other` purpose source for it has no production form; one
Tools edit grants every Agent that selects the document, so no production edit
is a single group spanning two Agents. -/
def overlappingGroupInputs : List GroupInput :=
  let initial : Configuration.ToolGrantProjection :=
    { enabled := schemaToolSources ++ [otherPurposeSource],
      grants := schemaToolSources ++ [otherPurposeSource],
      direct := [(agentDid, "get-collection")] }
  [groupRow "schema-disable-preserves-overlap" initial schemaToolSources false true true,
   groupRow "schema-disable-rollback" initial schemaToolSources false true false,
   groupRow "mixed-actor-rejected" {} [toolSource, otherAgentSource] true true true]

/-- `enabled` and `grants` are the group's tool sources alone; the Agent's
projected grant sets are sources outside every tool group. `permissions` is
what the live Agent holds: the group's effective grants and its baseline,
which keeps `get-collection` whatever the group does
(`GrantSets.get_collection_survives_tool_removal`). -/
def groupOutput (i : GroupInput) : GroupOutput :=
  let result := Configuration.changeToolGrantGroup i.initial.toModel (i.sources.map (·.toModel))
    i.enable i.authorized i.committed
  { enabled := result.enabled.map .ofModel, grants := result.grants.map .ofModel,
    permissions := Configuration.schemaToolPermissions.map fun permission =>
      ⟨permission, Configuration.toolGrantEffective result agentDid permission ||
        GrantSets.GrantSet.agentBaseline.permissions.contains permission⟩ }

/-- A group disable keeps an overlapping source of another purpose, a failed
disable keeps the whole group, and a group spanning two Agents is refused. -/
theorem group_change_keeps_what_it_does_not_name :
    overlappingGroupInputs.map (fun i => (groupOutput i).enabled.length) =
      [1, schemaToolSources.length + 1, 0] := by
  native_decide

structure GroupWire where
  toolsDoc : String
  purpose : String
  deriving ToJson

structure PermissionEdgeWire where
  group : GroupWire
  permission : String
  deriving ToJson

structure ContextEdgeWire where
  group : GroupWire
  contextDoc : String
  deriving ToJson

structure AgentEdgeWire where
  contextDoc : String
  agent : String
  deriving ToJson

/-- A hierarchy with every edge list in one canonical order, since the native
relations it is read back from are unordered sets. -/
structure HierarchyWire where
  permissions : List PermissionEdgeWire
  contexts : List ContextEdgeWire
  agents : List AgentEdgeWire
  direct : List DirectWire
  owners : List String
  admins : List String
  deriving ToJson

def canonical {α : Type} (key : α → String) (items : List α) : List α :=
  items.mergeSort fun a b => !decide (key b < key a)

def HierarchyWire.ofModel (state : Configuration.ToolGrantHierarchy) : HierarchyWire :=
  let group := fun (g : Configuration.ToolGroup) => (⟨g.toolsDoc, g.purpose⟩ : GroupWire)
  { permissions := canonical (fun e => e.group.toolsDoc ++ "|" ++ e.group.purpose ++ "|" ++ e.permission)
      (state.permissions.map fun e => ⟨group e.group, e.permission⟩),
    contexts := canonical (fun e => e.group.toolsDoc ++ "|" ++ e.group.purpose ++ "|" ++ e.contextDoc)
      (state.contexts.map fun e => ⟨group e.group, e.contextDoc⟩),
    agents := canonical (fun e => e.contextDoc ++ "|" ++ e.agent)
      (state.agents.map fun e => ⟨e.contextDoc, e.agent⟩),
    direct := canonical (fun e => e.actor ++ "|" ++ e.permission) (state.direct.map .ofModel),
    owners := canonical id state.owners, admins := canonical id state.admins }

def HierarchyWire.toModel (state : HierarchyWire) : Configuration.ToolGrantHierarchy :=
  let group := fun (g : GroupWire) => (⟨g.toolsDoc, g.purpose⟩ : Configuration.ToolGroup)
  { permissions := state.permissions.map fun e => ⟨group e.group, e.permission⟩,
    contexts := state.contexts.map fun e => ⟨group e.group, e.contextDoc⟩,
    agents := state.agents.map fun e => ⟨e.contextDoc, e.agent⟩,
    direct := state.direct.map fun grant => (grant.actor, grant.permission),
    owners := state.owners, admins := state.admins }

def sourceKey (source : SourceWire) : String :=
  source.toolsDoc ++ "|" ++ source.agent ++ "|" ++ source.purpose ++ "|" ++ source.permission

/-- The shared hierarchy: the Tools document `tools-doc` enables every schema
permission for its schema group, the Context `context-doc` selects that group,
and the Agent and its sibling both select `context-doc`. An `enable`/`disable`
edits the group's permissions, `context` moves `context-doc` to the schema
group of `next-tools-doc`, and `agent` moves the Agent to `next-context-doc`. -/
structure HierarchyInput where
  name : String
  action : String
  group : GroupWire
  nextGroup : GroupWire
  contextDoc : String
  nextContextDoc : String
  agent : String
  sibling : String
  initial : HierarchyWire
  authorized : Bool
  committed : Bool
  deriving ToJson

structure HierarchyOutput where
  hierarchy : HierarchyWire
  grants : List SourceWire
  agentAllowed : Bool
  siblingAllowed : Bool
  deriving ToJson

def hierarchyInputs : List HierarchyInput :=
  let inactive : Configuration.ToolGrantHierarchy := { sharedToolHierarchy with permissions := [] }
  let direct : Configuration.ToolGrantHierarchy :=
    { sharedToolHierarchy with direct := [(agentDid, "patch-collection")] }
  let row := fun (name action : String) (initial : Configuration.ToolGrantHierarchy)
      (authorized committed : Bool) =>
    ({ name, action, group := ⟨sharedToolGroup.toolsDoc, sharedToolGroup.purpose⟩,
       nextGroup := ⟨nextToolGroup.toolsDoc, nextToolGroup.purpose⟩, contextDoc := "context-doc",
       nextContextDoc := "next-context-doc", agent := agentDid, sibling := siblingDid,
       initial := .ofModel initial, authorized, committed } : HierarchyInput)
  [row "disable-shared-tools" "disable" sharedToolHierarchy true true,
   row "enable-dormant-shared-tools" "enable" inactive true true,
   row "disable-keeps-independent-grant" "disable" direct true true,
   row "failed-shared-disable" "disable" sharedToolHierarchy true false,
   row "unauthorized-shared-enable" "enable" inactive false true,
   row "move-shared-context" "context" sharedToolHierarchy true true,
   row "failed-context-move" "context" sharedToolHierarchy true false,
   row "move-one-agent" "agent" sharedToolHierarchy true true,
   row "unauthorized-agent-move" "agent" sharedToolHierarchy false true]

def hierarchyResult (i : HierarchyInput) : Configuration.ToolGrantHierarchy :=
  let initial := i.initial.toModel
  if i.action == "context" then
    Configuration.rebindToolContext initial i.contextDoc ⟨i.nextGroup.toolsDoc, i.nextGroup.purpose⟩
      i.authorized i.committed
  else if i.action == "agent" then
    Configuration.rebindToolAgent initial i.agent i.nextContextDoc i.authorized i.committed
  else
    Configuration.setToolHierarchy initial ⟨i.group.toolsDoc, i.group.purpose⟩
      Configuration.schemaToolPermissions (i.action == "enable") i.authorized i.committed

def hierarchyOutput (i : HierarchyInput) : HierarchyOutput :=
  let result := hierarchyResult i
  let flattened := Configuration.flattenToolHierarchy result
  { hierarchy := .ofModel result,
    grants := canonical sourceKey (flattened.grants.map .ofModel),
    agentAllowed := Configuration.toolGrantEffective flattened i.agent "patch-collection",
    siblingAllowed := Configuration.toolGrantEffective flattened i.sibling "patch-collection" }

structure RebindingInput where
  name : String
  old : SourceWire
  next : SourceWire
  initial : ProjectionWire
  authorized : Bool
  committed : Bool
  deriving ToJson

/-- The Agent's one Context selects `tools-doc`, whose schema source projects
`patch-collection`; a rebinding moves the Context to `other-tools-doc`, which
enables the same schema tool. -/
def rebindingInputs : List RebindingInput :=
  let initial : Configuration.ToolGrantProjection := { enabled := [toolSource], grants := [toolSource] }
  [("rebind", true, true), ("unauthorized-rebind", false, true), ("failed-rebind", true, false)].map
    fun (name, authorized, committed) =>
      { name, old := .ofModel toolSource, next := .ofModel otherToolSource,
        initial := .ofModel initial, authorized, committed }

def rebindingOutput (i : RebindingInput) : ProjectionOutput :=
  let result := Configuration.replaceToolGrant i.initial.toModel i.old.toModel i.next.toModel
    i.authorized i.committed
  { enabled := result.enabled.map .ofModel, grants := result.grants.map .ofModel,
    effective := Configuration.toolGrantEffective result agentDid "patch-collection" }

end Conformance.HouseholdC

namespace Conformance.Household

open Lean Identity Conformance.HouseholdC

@[conformance_family]
def toolGrantProjection : Family ProjectionInput ProjectionOutput where
  inputs := projectionInputs
  model := projectionOutput

@[conformance_family]
def toolGrantGroup : Family GroupInput GroupOutput where
  inputs := groupInputs
  model := groupOutput

@[conformance_family]
def toolGrantHierarchy : Family HierarchyInput HierarchyOutput where
  inputs := hierarchyInputs
  model := hierarchyOutput

/-- The rebinding scenario edits the Context's `tools_id` as the Node's owner,
as an Agent holding only the Context's `writer` relation, and as the owner in a
transaction that rolls back. -/
@[conformance_family]
def toolGrantRebinding : Family RebindingInput ProjectionOutput where
  inputs := rebindingInputs
  model := rebindingOutput

end Conformance.Household
