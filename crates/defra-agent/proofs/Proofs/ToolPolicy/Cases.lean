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

structure SurfaceView where
  fileRank : Nat
  meta : Bool
  defraQuery : Bool
  spawn : Bool
  bashMode : Nat
  bashNet : Nat
  bashSandbox : Bool
  bashAllowedKind : String
  bashAllowedPrefixes : List (List String)
  mcpProbe : String
  mcpScopeKind : String
  mcpServices : List String
  mcpPermits : Bool
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

def knownFieldNames : List String :=
  ["field_a", "field_b", "field_c"]

def fieldList (fields : Finset String) : List String :=
  knownFieldNames.filter (fun field => decide (field ∈ fields))

def toolScopeKeys {V : Type} : EndpointScope ToolId V → List String
  | .only keys _ => knownToolIds.filter (fun key => decide (key ∈ keys))
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
  , spawn := s.spawn
  , bashMode := s.bash.mode.rank
  , bashNet := s.bash.network.rank
  , bashSandbox := s.bash.sandbox
  , bashAllowedKind := scopeKind s.bash.allowed
  , bashAllowedPrefixes := bashAllowedPrefixes s.bash.allowed
  , mcpProbe := mcpProbe
  , mcpScopeKind := scopeKind s.mcpServices
  , mcpServices := toolScopeKeys s.mcpServices
  , mcpPermits := decide (s.mcpServices.permits mcpProbe)
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
  surface .readWrite
    (bashPolicy .unrestricted .enabled allowedOnlyGit)
    true true true (toolOnly "svc-x") writeA

def ceilingDisjointOnly : Surface :=
  surface .readWrite
    (bashPolicy .unrestricted .enabled allowedOnlyLs)
    true true true (toolOnly "svc-y") writeA

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
  ]

end ToolPolicy.ContractCases
