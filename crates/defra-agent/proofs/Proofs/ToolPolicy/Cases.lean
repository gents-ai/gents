import Proofs.ToolPolicy.Instances

/-!
# Tool Policy Contract Cases

Small executable witnesses carrying behavior, ceiling, runtime inputs and the
Lean-computed effective output.
-/

namespace ToolPolicy.ContractCases

open ToolPolicy

structure SurfaceView where
  fileRank : Nat
  meta : Bool
  defraQuery : Bool
  spawn : Bool
  bashMode : Nat
  bashNet : Nat
  bashSandbox : Bool
  bashAllowedKind : String
  mcpProbe : String
  mcpPermits : Bool
  writeProbe : String × String
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

def unitOnly {K : Type} (keys : Finset K) : EndpointScope K Unit :=
  .only keys (fun _ => ())

def toolOnly (tool : ToolId) : EndpointScope ToolId Unit :=
  unitOnly [tool].toFinset

def writeOnly (key : String × String) (fields : List String) :
    EndpointScope (String × String) (Finset String) :=
  .only [key].toFinset (fun _ => stringSet fields)

def fieldList (fields : Finset String) : List String :=
  if "field_a" ∈ fields then
    ["field_a"]
  else if "field_b" ∈ fields then
    ["field_b"]
  else
    []

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
  , mcpProbe := mcpProbe
  , mcpPermits := decide (s.mcpServices.permits mcpProbe)
  , writeProbe := writeProbe
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
  , mkCase "disjoint_only_scopes_intersect_to_empty"
      behaviorDisjointOnly ceilingDisjointOnly wideOpen "svc-x" probeWrite
  , mkCase "bash_all_allowed_kind_idempotent"
      wideOpen wideOpen wideOpen "svc-a" probeWrite
  ]

end ToolPolicy.ContractCases
