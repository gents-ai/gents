import Proofs.ToolPolicy.Instances

/-!
# Tool Policy Contract Cases

Small executable witnesses carrying behavior, ceiling, runtime inputs and the
Lean-computed effective output.
-/

namespace ToolPolicy.ContractCases

open ToolPolicy

structure WriteGrantView where
  tool : String
  collection : String
  fields : List String
  deriving Repr

/-- Projection of a `Surface` carrying EVERY document-driven category
    independently, so the conformance round-trip fences the production meet for
    all 19 categories (not just the original 7). Each boolean capability and each
    `EndpointScope` (kind + surviving keys) is observed directly — no category is
    aliased to another, so a `||`-vs-`&&` typo or a wrong scope branch in any
    single category diverges from the Lean-computed `expected`. -/
structure SurfaceView where
  fileRank : Nat
  meta : Bool
  defraQuery : Bool
  memory : Bool
  sessionHistory : Bool
  contextBudget : Bool
  spawn : Bool
  steering : Bool
  background : Bool
  orchestration : Bool
  crossDeployment : Bool
  skills : Bool
  bashMode : Nat
  bashNet : Nat
  bashSandbox : Bool
  bashAllowedKind : String
  bashAllowedPrefixes : List (List String)
  cliScopeKind : String
  cliKeys : List String
  mcpProbe : String
  mcpScopeKind : String
  mcpServices : List String
  mcpPermits : Bool
  defraCollectionsScopeKind : String
  defraCollectionsKeys : List String
  subagentTargetsScopeKind : String
  subagentTargetsKeys : List String
  backgroundToolsScopeKind : String
  backgroundToolsKeys : List String
  writeProbe : String × String
  writeScopeKind : String
  writeGrants : List WriteGrantView
  writeFields : List String
  deriving Repr

structure Case where
  name : String
  behavior : SurfaceView
  ceiling : SurfaceView
  runtime : SurfaceView
  expected : SurfaceView
  deriving Repr

def scopeKind {K V : Type} : EndpointScope K V → String
  | .all => "all"
  | .only _ _ => "only"
  | .none => "none"

def stringSet (items : List String) : Finset String :=
  items.toFinset

def knownToolIds : List String :=
  ["svc-a", "svc-x", "svc-y"]

def knownArgvPrefixes : List (List String) :=
  [["git", "status"], ["ls"]]

def knownWriteKeys : List (String × String) :=
  [("wt", "coll"), ("wt", "coll1"), ("wt", "coll2")]

-- Kept sorted so the filtered projection matches the production `BTreeMap`
-- key order on the Rust side (the round-trip compares the lists verbatim).
def knownSubagentTargets : List (String × String) :=
  [("did-a", "beh-a"), ("did-b", "beh-b")]

def knownFieldNames : List String :=
  ["field_a", "field_b", "field_c"]

def fieldList (fields : Finset String) : List String :=
  knownFieldNames.filter (fun field => decide (field ∈ fields))

def toolScopeKeys {V : Type} : EndpointScope ToolId V → List String
  | .only keys _ => knownToolIds.filter (fun key => decide (key ∈ keys))
  | .all => []
  | .none => []

-- Pair keys are observed as `"<did>::<behavior>"` strings; the separator is
-- absent from the controlled test keys, so the encoding is injective here and
-- matches the Rust mirror's `format!("{did}::{behavior}")`.
def subagentScopeKeys {V : Type} : EndpointScope (String × String) V → List String
  | .only keys _ =>
      knownSubagentTargets.filterMap (fun key =>
        if key ∈ keys then some (key.1 ++ "::" ++ key.2) else none)
  | .all => []
  | .none => []

def bashAllowedPrefixes : EndpointScope (List String) Unit → List (List String)
  | .only keys _ => knownArgvPrefixes.filter (fun key => decide (key ∈ keys))
  | .all => []
  | .none => []

def writeGrantViews :
    EndpointScope (String × String) (Finset String) → List WriteGrantView
  | .only keys val =>
      knownWriteKeys.filterMap (fun key =>
        if key ∈ keys then
          some
            { tool := key.1
            , collection := key.2
            , fields := fieldList (val key) }
        else
          none)
  | .all => []
  | .none => []

def unitOnly {K : Type} (keys : Finset K) : EndpointScope K Unit :=
  .only keys (fun _ => ())

def toolOnly (tool : ToolId) : EndpointScope ToolId Unit :=
  unitOnly [tool].toFinset

def toolsOnly (tools : List ToolId) : EndpointScope ToolId Unit :=
  unitOnly tools.toFinset

def subagentOnly (keys : List (String × String)) :
    EndpointScope (String × String) Unit :=
  unitOnly keys.toFinset

def cliOnly (entries : List (String × List String)) :
    EndpointScope ToolId (Finset String) :=
  .only (entries.map Prod.fst).toFinset
    (fun key =>
      match entries.find? (fun entry => entry.1 == key) with
      | some entry => stringSet entry.2
      | none => ∅)

def writeOnly (key : String × String) (fields : List String) :
    EndpointScope (String × String) (Finset String) :=
  .only [key].toFinset (fun _ => stringSet fields)

def bashPolicy (mode : ExecMode) (network : NetMode)
    (allowed : EndpointScope (List String) Unit) : BashPolicy :=
  { mode := mode
  , network := network
  , forbidden := ∅
  , allowed := allowed
  , readOnly := .all
  , sandbox := true }

def surface (file : FileCap) (bash : BashPolicy)
    (meta defraQuery spawn : Bool)
    (mcp : EndpointScope ToolId Unit)
    (write : EndpointScope (String × String) (Finset String)) : Surface :=
  { file := file
  , bash := bash
  , meta := meta
  , defraQuery := defraQuery
  , memory := meta
  , sessionHistory := meta
  , contextBudget := meta
  , spawn := spawn
  , steering := meta
  , background := spawn
  , orchestration := spawn
  , crossDeployment := spawn
  , skills := meta
  , cliTools := .all
  , mcpServices := mcp
  , defraCollections := .all
  , subagentTargets := .all
  , backgroundTools := .all
  , writeTools := write }

def view (s : Surface) (mcpProbe : String) (writeProbe : String × String) : SurfaceView :=
  { fileRank := s.file.rank
  , meta := s.meta
  , defraQuery := s.defraQuery
  , memory := s.memory
  , sessionHistory := s.sessionHistory
  , contextBudget := s.contextBudget
  , spawn := s.spawn
  , steering := s.steering
  , background := s.background
  , orchestration := s.orchestration
  , crossDeployment := s.crossDeployment
  , skills := s.skills
  , bashMode := s.bash.mode.rank
  , bashNet := s.bash.network.rank
  , bashSandbox := s.bash.sandbox
  , bashAllowedKind := scopeKind s.bash.allowed
  , bashAllowedPrefixes := bashAllowedPrefixes s.bash.allowed
  , cliScopeKind := scopeKind s.cliTools
  , cliKeys := toolScopeKeys s.cliTools
  , mcpProbe := mcpProbe
  , mcpScopeKind := scopeKind s.mcpServices
  , mcpServices := toolScopeKeys s.mcpServices
  , mcpPermits := decide (s.mcpServices.permits mcpProbe)
  , defraCollectionsScopeKind := scopeKind s.defraCollections
  , defraCollectionsKeys := toolScopeKeys s.defraCollections
  , subagentTargetsScopeKind := scopeKind s.subagentTargets
  , subagentTargetsKeys := subagentScopeKeys s.subagentTargets
  , backgroundToolsScopeKind := scopeKind s.backgroundTools
  , backgroundToolsKeys := toolScopeKeys s.backgroundTools
  , writeProbe := writeProbe
  , writeScopeKind := scopeKind s.writeTools
  , writeGrants := writeGrantViews s.writeTools
  , writeFields := match s.writeTools.lookup writeProbe with
      | some fields => fieldList fields
      | none => [] }

def probeWrite : String × String := ("wt", "coll")

def allowedOnlyGit : EndpointScope (List String) Unit :=
  unitOnly [["git", "status"]].toFinset

def allowedOnlyLs : EndpointScope (List String) Unit :=
  unitOnly [["ls"]].toFinset

def writeA : EndpointScope (String × String) (Finset String) :=
  writeOnly probeWrite ["field_a"]

def writeB : EndpointScope (String × String) (Finset String) :=
  writeOnly probeWrite ["field_b"]

def writeEmpty : EndpointScope (String × String) (Finset String) :=
  writeOnly probeWrite []

def wideOpen : Surface :=
  surface .readWrite
    (bashPolicy .unrestricted .enabled .all)
    true true true .all writeA

def secureMinimal : Surface :=
  surface .off
    (bashPolicy .readOnly .inherit .none)
    false false false .none writeEmpty

def ceilingMcpOnly : Surface :=
  surface .readWrite
    (bashPolicy .unrestricted .enabled .all)
    true true true (toolOnly "svc-a") writeA

def runtimeNoMcp : Surface :=
  surface .readWrite
    (bashPolicy .unrestricted .enabled .all)
    true true true .none writeA

def behaviorWriteB : Surface :=
  surface .readWrite
    (bashPolicy .unrestricted .enabled .all)
    true true true .all writeB

def ceilingWriteFieldsNarrowed : Surface :=
  surface .readWrite
    (bashPolicy .unrestricted .enabled .all)
    true true true .all writeA

-- Same write tool, DIFFERENT collection: behavior grants `(wt, coll1)`, ceiling
-- grants `(wt, coll2)`. Because the collection is part of the KEY, the meet
-- intersects to an empty key set — the write tool is DENIED, not merged. Guards
-- against tool-name-only keying, which would silently keep it active.
def writeCollA : EndpointScope (String × String) (Finset String) :=
  writeOnly ("wt", "coll1") ["field_a"]

def writeCollB : EndpointScope (String × String) (Finset String) :=
  writeOnly ("wt", "coll2") ["field_a"]

def behaviorWriteCollA : Surface :=
  surface .readWrite
    (bashPolicy .unrestricted .enabled .all)
    true true true .all writeCollA

def ceilingWriteCollB : Surface :=
  surface .readWrite
    (bashPolicy .unrestricted .enabled .all)
    true true true .all writeCollB

-- Two disjoint, non-empty `.only` scopes on both bash-allowed and mcp:
-- behavior permits `svc-x` / `git status`, ceiling permits `svc-y` / `ls`.
-- Their meet intersects to empty, exercising the `only ∩ only` branch and the
-- `Only(∅)` deny-all trap (which must serialize as "only", never "all").
def behaviorDisjointOnly : Surface :=
  { surface .readWrite
      (bashPolicy .unrestricted .enabled allowedOnlyGit)
      true true true (toolOnly "svc-x") writeA with
    defraCollections := toolOnly "svc-x" }

def ceilingDisjointOnly : Surface :=
  { surface .readWrite
      (bashPolicy .unrestricted .enabled allowedOnlyLs)
      true true true (toolOnly "svc-y") writeA with
    defraCollections := toolOnly "svc-y" }

-- Behavior with EVERY boolean capability on and every keyed scope a broad
-- `Only`. Paired with `ceilingClampsEachCategory` it fences all 12 categories
-- that the original 7-category view aliased away: the 8 booleans must clamp to
-- false, and the 4 scopes must key-intersect down to the ceiling's narrow set.
def behaviorEachCategory : Surface :=
  { wideOpen with
    cliTools := cliOnly [("svc-a", ["field_a", "field_b"]), ("svc-x", ["field_a"])]
  , defraCollections := toolsOnly ["svc-a", "svc-x"]
  , subagentTargets := subagentOnly [("did-a", "beh-a"), ("did-b", "beh-b")]
  , backgroundTools := toolsOnly ["svc-a", "svc-x"] }

def ceilingClampsEachCategory : Surface :=
  { wideOpen with
    memory := false
  , sessionHistory := false
  , contextBudget := false
  , steering := false
  , background := false
  , orchestration := false
  , crossDeployment := false
  , skills := false
  , cliTools := cliOnly [("svc-a", ["field_a"])]
  , defraCollections := toolOnly "svc-a"
  , subagentTargets := subagentOnly [("did-a", "beh-a")]
  , backgroundTools := toolOnly "svc-a" }

-- Behavior leaves the four keyed scopes wide open (`All`); the ceiling narrows
-- each to a small `Only`. Fences the `All ⊓ Only = Only` branch — and the dual
-- direction from `behaviorEachCategory` — for cli/defra/subagent/background,
-- while the booleans stay true (no spurious clamp).
def ceilingScopesOnly : Surface :=
  { wideOpen with
    cliTools := cliOnly [("svc-a", ["field_a"])]
  , defraCollections := toolOnly "svc-a"
  , subagentTargets := subagentOnly [("did-a", "beh-a")]
  , backgroundTools := toolOnly "svc-a" }

def mkCase (name : String) (b c : Surface) (r : Avail)
    (mcpProbe : String) (writeProbe : String × String) : Case :=
  { name := name
  , behavior := view b mcpProbe writeProbe
  , ceiling := view c mcpProbe writeProbe
  , runtime := view r mcpProbe writeProbe
  , expected := view (effective b c r) mcpProbe writeProbe }

def cases : List Case :=
  [ mkCase "wide_open_clamped_by_secure_ceiling"
      wideOpen secureMinimal wideOpen "svc-a" probeWrite
  , mkCase "ceiling_mcp_only_clamps_behavior"
      wideOpen ceilingMcpOnly wideOpen "svc-a" probeWrite
  , mkCase "runtime_offline_drops_permitted_mcp"
      wideOpen wideOpen runtimeNoMcp "svc-a" probeWrite
  , mkCase "write_fields_narrowed_by_ceiling"
      behaviorWriteB ceilingWriteFieldsNarrowed wideOpen "svc-a" probeWrite
  , mkCase "write_tool_collection_mismatch_denies"
      behaviorWriteCollA ceilingWriteCollB wideOpen "svc-a" ("wt", "coll1")
  , mkCase "disjoint_only_scopes_intersect_to_empty"
      behaviorDisjointOnly ceilingDisjointOnly wideOpen "svc-x" probeWrite
  , mkCase "bash_all_allowed_kind_idempotent"
      wideOpen wideOpen wideOpen "svc-a" probeWrite
  , mkCase "ceiling_clamps_each_category"
      behaviorEachCategory ceilingClampsEachCategory wideOpen "svc-a" probeWrite
  , mkCase "behavior_all_scopes_clamped_by_ceiling_only"
      wideOpen ceilingScopesOnly wideOpen "svc-a" probeWrite
  ]

end ToolPolicy.ContractCases
