import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family
import Proofs.Tools.ToolPolicy.Cases
import Proofs.Tools.ToolPolicy.FieldRead
import Proofs.Shared.ToolPolicy.WriteInput
import Proofs.Tools.ToolPolicy.ApplicationWrite

import Proofs.Tools.ToolPolicy.WorkspacePermissions

namespace Conformance.Contracts

private def permissionPreferenceJson (value : ToolPolicy.WorkspacePermissions.Preference) : String :=
  "{\"revision\":" ++ toString value.revision
  ++ ",\"mode\":" ++ jsonString value.mode.wireName
  ++ ",\"reviewTask\":" ++ (match value.reviewTask with | none => "null" | some task => toString task) ++ "}"

private def permissionOptionalPreferenceJson (value : Option ToolPolicy.WorkspacePermissions.Preference) : String :=
  match value with
  | none => "null"
  | some value => permissionPreferenceJson value

def workspacePermissionWriteCasesJson : String :=
  jsonArray (ToolPolicy.WorkspacePermissions.writeCases.map fun witness =>
    let result := ToolPolicy.WorkspacePermissions.selectMode witness.previous witness.expected witness.mode witness.requestedTask
    "{\"previous\":" ++ permissionOptionalPreferenceJson witness.previous
    ++ ",\"expected\":" ++ toString witness.expected
    ++ ",\"mode\":" ++ jsonString witness.mode.wireName
    ++ ",\"requestedTask\":" ++ (match witness.requestedTask with | none => "null" | some task => toString task)
    ++ ",\"accepted\":" ++ (if result.accepted then "true" else "false")
    ++ ",\"changed\":" ++ (if result.changed then "true" else "false")
    ++ ",\"next\":" ++ permissionOptionalPreferenceJson result.next ++ "}")

def permissionScopeJson (scope : ToolPolicy.WorkspacePermissions.Scope) : Lean.Json :=
  Lean.Json.mkObj [("workspace", Lean.toJson scope.workspace), ("revision", Lean.toJson scope.revision)]

def permissionGrantJson (grant : ToolPolicy.WorkspacePermissions.Grant) : Lean.Json :=
  Lean.Json.mkObj [("scope", permissionScopeJson grant.scope), ("action", Lean.toJson grant.action)]

def permissionInputJson (input : ToolPolicy.WorkspacePermissions.Input) : Lean.Json :=
  Lean.Json.mkObj [
    ("current", permissionScopeJson input.current),
    ("configuredWorkspace", Lean.toJson input.configuredWorkspace),
    ("mode", Lean.toJson input.mode.wireName), ("action", Lean.toJson input.action),
    ("ceilingAllows", Lean.toJson input.ceilingAllows),
    ("crossesBoundary", Lean.toJson input.crossesBoundary),
    ("grant", match input.grant with | none => .null | some grant => permissionGrantJson grant)]

def workspacePermissionCasesJson : String :=
  jsonArray (ToolPolicy.WorkspacePermissions.cases.map fun witness =>
    let input := witness.input
    "{\"input\":" ++ (permissionInputJson input).compress
    ++ ",\"review\":" ++ jsonString witness.review.wireName
    ++ ",\"reviewedGrant\":" ++ (permissionGrantJson witness.reviewedGrant).compress
    ++ ",\"expectedRoute\":" ++ jsonString (ToolPolicy.WorkspacePermissions.route input).wireName
    ++ ",\"expectedReviewedRoute\":" ++ jsonString (ToolPolicy.WorkspacePermissions.reviewedRoute input witness.review witness.reviewedGrant).wireName
    ++ ",\"expectedValidGrant\":" ++ (if ToolPolicy.WorkspacePermissions.validGrant input then "true" else "false") ++ "}")

open ToolPolicy.ContractCases

def boolJson (b : Bool) : String := if b then "true" else "false"

structure FieldReadInput where
  widths : List Nat
  offset : Nat
  /-- The page byte budget; production reads `MAX_FIELD_STRING_BYTES`, and
  a returned page never exceeds it. -/
  budget : Nat
  granted : Bool
  present : Bool
  hashOk : Bool
  deriving Lean.ToJson

@[conformance_family]
def fieldRead : Family FieldReadInput (Option Nat) where
  inputs :=
    [[], [1], [1, 2, 3, 4], List.replicate 2000 1 ++ [4], List.replicate 501 4].flatMap fun widths =>
      [0, 1, 2, 3, 6, 10, 2000, 2004, 2005].flatMap fun offset =>
        [false, true].flatMap fun present =>
          [false, true].flatMap fun hashOk =>
            [false, true].map fun granted =>
              { widths, offset, budget := 2000, granted, present, hashOk }
  model := fun i =>
    ToolPolicy.FieldRead.page i.widths i.offset i.budget i.granted i.granted i.granted
      i.present i.hashOk

deriving instance Lean.ToJson for ToolPolicy.WriteInput.Kind

structure WriteInputCase where
  expected : ToolPolicy.WriteInput.Kind
  actual : Option ToolPolicy.WriteInput.Kind
  nullable : Bool
  required : Bool
  filled : Bool
  deriving Lean.ToJson

def writeInputCases : List WriteInputCase :=
  [ToolPolicy.WriteInput.Kind.text, .integer, .number, .boolean].flatMap fun expected =>
    [none, some .text, some .integer, some .number, some .boolean,
        some .array, some .object, some .null].flatMap fun actual =>
      [false, true].flatMap fun nullable =>
        [false, true].flatMap fun required =>
          [false, true].map fun filled => { expected, actual, nullable, required, filled }

def writeInputAccepted (c : WriteInputCase) : Bool :=
  ToolPolicy.WriteInput.admits c.expected c.nullable c.required c.filled c.actual

@[conformance_family]
def writeInput : Family WriteInputCase Bool where
  inputs := writeInputCases
  model := writeInputAccepted

/-- A bounded query's filter field is a declared, required, caller-supplied
argument over a non-nullable collection field: the query tool has no nullable,
optional or runtime-filled filter to build the other cases from. -/
@[conformance_family]
def writeInputQueryFilter : Family WriteInputCase Bool where
  inputs := writeInputCases.filter fun c => !c.nullable && c.required && !c.filled
  model := writeInputAccepted

/-- A plugin's bound input is always runtime-filled from the trigger context:
the plugin tool has no caller-supplied bound input to build the other cases
from. -/
@[conformance_family]
def writeInputPluginFill : Family WriteInputCase Bool where
  inputs := writeInputCases.filter fun c => c.filled
  model := writeInputAccepted

structure InvocationCorrelationInput where
  request : Option String
  supplied : Option String
  deriving Lean.ToJson

@[conformance_family]
def invocationCorrelation : Family InvocationCorrelationInput (Option String) where
  inputs := [none, some "", some " ", some "request-1"].flatMap fun request =>
    [none, some "", some " ", some "event-1"].map fun supplied => { request, supplied }
  model := fun i => ToolPolicy.WriteInput.invocationCorrelation i.request i.supplied

deriving instance Lean.ToJson for WriteGrantView

/-- The wire form of `SurfaceView`: its probes are pairs, which the wire
carries as two fields. -/
structure PolicyView where
  fileRank : Nat
  goalTools : Bool
  goalCreate : Bool
  defraQuery : Bool
  selfConfig : Bool
  schemaManagement : Bool
  p2pRead : Bool
  p2pMutate : Bool
  p2pMutationAllowed : Bool
  sessionHistory : Bool
  contextBudget : Bool
  sessionMessages : Bool
  skills : Bool
  lsp : Bool
  bashMode : Nat
  bashNet : Nat
  bashSandbox : Bool
  bashAllowedKind : String
  bashAllowedPrefixes : List (List String)
  bashForbidden : List (List String)
  bashReadOnlyKind : String
  bashReadOnlyKeys : List String
  cliScopeKind : String
  cliKeys : List String
  mcpProbe : String
  mcpScopeKind : String
  mcpServices : List String
  mcpPermits : Bool
  defraCollectionsScopeKind : String
  defraCollectionsKeys : List String
  selfConfigCategoriesScopeKind : String
  selfConfigCategoriesKeys : List String
  agentTargetsScopeKind : String
  agentTargetsKeys : List String
  backgroundToolsScopeKind : String
  backgroundToolsKeys : List String
  writeProbeTool : String
  writeProbeCollection : String
  writeScopeKind : String
  writeGrants : List WriteGrantView
  writeFields : List String
  queryProbeTool : String
  queryProbeCollection : String
  queryScopeKind : String
  queryGrants : List WriteGrantView
  queryFields : List String
  pluginToolsKind : String
  pluginToolsKeys : List String
  deriving Lean.ToJson

def PolicyView.ofView (v : SurfaceView) : PolicyView :=
  { fileRank := v.fileRank
  , goalTools := v.goalTools
  , goalCreate := v.goalCreate
  , defraQuery := v.defraQuery
  , selfConfig := v.selfConfig
  , schemaManagement := v.schemaManagement
  , p2pRead := v.p2pRead
  , p2pMutate := v.p2pMutate
  , p2pMutationAllowed := v.p2pMutationAllowed
  , sessionHistory := v.sessionHistory
  , contextBudget := v.contextBudget
  , sessionMessages := v.sessionMessages
  , skills := v.skills
  , lsp := v.lsp
  , bashMode := v.bashMode
  , bashNet := v.bashNet
  , bashSandbox := v.bashSandbox
  , bashAllowedKind := v.bashAllowedKind
  , bashAllowedPrefixes := v.bashAllowedPrefixes
  , bashForbidden := v.bashForbidden
  , bashReadOnlyKind := v.bashReadOnlyKind
  , bashReadOnlyKeys := v.bashReadOnlyKeys
  , cliScopeKind := v.cliScopeKind
  , cliKeys := v.cliKeys
  , mcpProbe := v.mcpProbe
  , mcpScopeKind := v.mcpScopeKind
  , mcpServices := v.mcpServices
  , mcpPermits := v.mcpPermits
  , defraCollectionsScopeKind := v.defraCollectionsScopeKind
  , defraCollectionsKeys := v.defraCollectionsKeys
  , selfConfigCategoriesScopeKind := v.selfConfigCategoriesScopeKind
  , selfConfigCategoriesKeys := v.selfConfigCategoriesKeys
  , agentTargetsScopeKind := v.agentTargetsScopeKind
  , agentTargetsKeys := v.agentTargetsKeys
  , backgroundToolsScopeKind := v.backgroundToolsScopeKind
  , backgroundToolsKeys := v.backgroundToolsKeys
  , writeProbeTool := v.writeProbe.1
  , writeProbeCollection := v.writeProbe.2
  , writeScopeKind := v.writeScopeKind
  , writeGrants := v.writeGrants
  , writeFields := v.writeFields
  , queryProbeTool := v.queryProbe.1
  , queryProbeCollection := v.queryProbe.2
  , queryScopeKind := v.queryScopeKind
  , queryGrants := v.queryGrants
  , queryFields := v.queryFields
  , pluginToolsKind := v.pluginToolsKind
  , pluginToolsKeys := v.pluginToolsKeys }

def fileOfRank : Nat → ToolPolicy.FileCap
  | 0 => .off
  | 1 => .readOnly
  | _ => .readWrite

def executionModeOfCode : Nat → CommandPolicy.ExecutionMode
  | 0 => .readOnly
  | 1 => .workspaceWrite
  | 2 => .unrestricted
  | _ => .artifactWrite

def networkOfRank : Nat → CommandPolicy.NetworkMode
  | 0 => .disabled
  | 1 => .inherit
  | _ => .enabled

def unitScopeOf {K : Type} [DecidableEq K] (kind : String) (keys : List K) :
    ToolPolicy.EndpointScope K Unit :=
  if kind == "all" then .all
  else if kind == "only" then .only keys.toFinset (fun _ => ())
  else .none

def cliScopeOf (kind : String) (keys : List String) :
    ToolPolicy.EndpointScope ToolPolicy.ToolId (Finset String) :=
  if kind == "all" then .all
  else if kind == "only" then .only keys.toFinset (fun _ => ∅)
  else .none

def grantScopeOf (kind : String) (grants : List WriteGrantView) :
    ToolPolicy.EndpointScope (String × String) (Finset String) :=
  if kind == "all" then .all
  else if kind == "only" then
    .only (grants.map fun g => (g.tool, g.collection)).toFinset fun key =>
      match grants.find? (fun g => (g.tool, g.collection) == key) with
      | some g => g.fields.toFinset
      | none => ∅
  else .none

def agentTargetsOf (keys : List String) : List (String × String) :=
  knownAgentTargets.filter fun key => (key.1 ++ "::" ++ key.2) ∈ keys

/-- The surface a wire view stands for: every scope key the cases use is one
of the known keys the view lists. -/
def PolicyView.surface (v : PolicyView) : ToolPolicy.Surface :=
  { file := fileOfRank v.fileRank
  , bash :=
      { mode := executionModeOfCode v.bashMode
      , network := networkOfRank v.bashNet
      , forbidden := v.bashForbidden.toFinset
      , allowed := unitScopeOf v.bashAllowedKind v.bashAllowedPrefixes
      , readOnly := unitScopeOf v.bashReadOnlyKind v.bashReadOnlyKeys
      , sandbox := v.bashSandbox }
  , goalTools := v.goalTools
  , goalCreate := v.goalCreate
  , defraQuery := v.defraQuery
  , selfConfig := v.selfConfig
  , schemaManagement := v.schemaManagement
  , p2pRead := v.p2pRead
  , p2pMutate := v.p2pMutate
  , sessionHistory := v.sessionHistory
  , contextBudget := v.contextBudget
  , sessionMessages := v.sessionMessages
  , skills := v.skills
  , lsp := v.lsp
  , cliTools := cliScopeOf v.cliScopeKind v.cliKeys
  , mcpServices := unitScopeOf v.mcpScopeKind v.mcpServices
  , defraCollections := unitScopeOf v.defraCollectionsScopeKind v.defraCollectionsKeys
  , selfConfigCategories :=
      unitScopeOf v.selfConfigCategoriesScopeKind v.selfConfigCategoriesKeys
  , agentTargets := unitScopeOf v.agentTargetsScopeKind (agentTargetsOf v.agentTargetsKeys)
  , backgroundTools := unitScopeOf v.backgroundToolsScopeKind v.backgroundToolsKeys
  , writeTools := grantScopeOf v.writeScopeKind v.writeGrants
  , queryTools := grantScopeOf v.queryScopeKind v.queryGrants
  , pluginTools := unitScopeOf v.pluginToolsKind v.pluginToolsKeys }

structure ToolPolicyCompositionInput where
  name : String
  agent : PolicyView
  ceiling : PolicyView
  runtime : PolicyView
  deriving Lean.ToJson

@[conformance_family]
def toolPolicyComposition : Family ToolPolicyCompositionInput PolicyView where
  inputs := ToolPolicy.ContractCases.cases.map fun c =>
    { name := c.name, agent := .ofView c.agent, ceiling := .ofView c.ceiling
      runtime := .ofView c.runtime }
  model := fun i =>
    .ofView (view (ToolPolicy.effective i.agent.surface i.ceiling.surface i.runtime.surface)
      i.agent.mcpProbe (i.agent.writeProbeTool, i.agent.writeProbeCollection))

structure GoalCapabilityResolutionCase where
  name : String
  explicitGoalTools : Option Bool
  explicitGoalCreate : Option Bool

def goalCapabilityResolutionCases : List GoalCapabilityResolutionCase :=
  [ ⟨"missing_goal_tools_is_off", none, none⟩
  , ⟨"explicit_goal_on", some true, none⟩
  , ⟨"explicit_goal_off", some false, none⟩
  , ⟨"creation_explicit_on", some true, some true⟩ ]

structure GoalCapabilityResolutionInput where
  name : String
  explicitGoalTools : Option Bool
  explicitGoalCreate : Option Bool
  deriving Lean.ToJson

structure GoalCapabilityResolutionOutput where
  goalTools : Bool
  goalCreate : Bool
  deriving Lean.ToJson

@[conformance_family]
def goalCapabilityResolution :
    Family GoalCapabilityResolutionInput GoalCapabilityResolutionOutput where
  inputs := goalCapabilityResolutionCases.map fun c =>
    { name := c.name, explicitGoalTools := c.explicitGoalTools
      explicitGoalCreate := c.explicitGoalCreate }
  model := fun i =>
    { goalTools := ToolPolicy.resolveGoalTools i.explicitGoalTools
      goalCreate := ToolPolicy.resolveGoalCreate i.explicitGoalCreate }

end Conformance.Contracts

namespace Conformance.Contracts
def applicationWriteCasesJson : String := Id.run do
  let mut rows := []
  for op in [ToolPolicy.ApplicationWrite.Operation.create, .update, .delete] do
    for granted in [false, true] do
      for application in [false, true] do
        for blocked in [false, true] do
          for credential in [false, true] do
            for bounded in [false, true] do
              for targets in [0, 1, 3, 101] do
                for limit in [0, 2, 100, 101] do
                  for preview in [false, true] do
                    for digestMatches in [false, true] do
                      let o : ToolPolicy.ApplicationWrite.Observation :=
                        ⟨granted, application, blocked, credential, bounded, targets, limit, preview, digestMatches⟩
                      let name := match op with | .create => "create" | .update => "update" | .delete => "delete"
                      rows := rows ++ ["{\"operation\":" ++ jsonString name
                        ++ ",\"granted\":" ++ boolJson granted ++ ",\"application\":" ++ boolJson application
                        ++ ",\"protected\":" ++ boolJson blocked ++ ",\"credential\":" ++ boolJson credential
                        ++ ",\"bounded\":" ++ boolJson bounded ++ ",\"targets\":" ++ toString targets
                        ++ ",\"limit\":" ++ toString limit ++ ",\"preview\":" ++ boolJson preview
                        ++ ",\"digest_matches\":" ++ boolJson digestMatches
                        ++ ",\"admitted\":" ++ boolJson (ToolPolicy.ApplicationWrite.admitted op o)
                        ++ ",\"may_apply\":" ++ boolJson (ToolPolicy.ApplicationWrite.mayApply op o) ++ "}"]
  return jsonArray rows
end Conformance.Contracts
