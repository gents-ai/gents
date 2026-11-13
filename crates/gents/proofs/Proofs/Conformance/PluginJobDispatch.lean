import Proofs.ToolCalls.PluginJob.CallingRow
import Proofs.ToolCalls.PluginJob.Adoption
import Proofs.Tools.ToolPolicy.Configuration
import Proofs.Conformance.Family

namespace Conformance.PluginJobDispatch

open Lean PluginJob ToolPolicy

private def worker : Target := ("node-a", "worker")

private def wireId (target : Target) : String := target.2 ++ "@" ++ target.1

private def scope (ts : List Target) : EndpointScope Target Unit := .only ts.toFinset fun _ => ()

private def ceiling : Ceiling :=
  { targets := [wireId worker], maxLive := 4, maxSpawns := 4, maxMessagesPerSlot := 2
  , resultBytes := 4000, maxWallSecs := 7200 }

private def facts : HostFacts :=
  { engineDigest := "engine-1", pluginRef := "team/plugin", cwd := "/work"
  , argsDigest := "args-1", chainRoot := "own", origin := .installed, adoptedAt := 0
  , resumeOf := none, replayedFrom := none, ceilingAtPin := ceiling }

private def guestPin : GuestPin :=
  { scriptId := "s", sourceDigest := "sha256:s", handle := "h", agentBudget := 2 }

private def callerScope : Background.ProcessControl.Scope :=
  { requestId := "request-1", sessionId := "session-1", nodeDid := "did:key:node"
  , requesterDid := none }

/-- The runtime facts of the job tool `workflow` of `team/plugin`, wired to
start `worker`, in the caller's session. `own` is a running job row of this
plugin in the session; `foreign` is a job row of another plugin there. -/
private def context : StepContext :=
  { live := ceiling, bindings := [⟨wireId worker, worker.1, worker.2⟩], available := true
  , callerTargets := scope [worker], jobTargets := scope [worker]
  , policy := { mode := .run, dynamicTargets := [wireId worker], trustedPacks := [] }
  , engineDigest := "engine-1", engineProvenanceOperator := true
  , maxHop := none, targetHop := fun _ => 0
  , cwdAllowed := fun _ => true, within := fun base cwd => base == cwd
  , liveBackground := 0, startedByJob := false, adoptionFacts := facts
  , callerScope
  , controlTarget := fun d =>
      if d == "own" then some (callerScope, true)
      else if d == "foreign" then some (callerScope, false) else none
  , resumeTarget := fun _ => none, resumeIsHead := fun _ => false, resumeFacts := facts }

private def stepZero (row : DocId) : Job :=
  { root := row, rowDoc := row, callerSession := "session-1", callerCwd := "/work"
  , handle := "h", callerHop := 0
  , call := { argv := ["run"], args := none, script := none, agentBudget := none }
  , rootCall := none, resume := none, pin := none, children := [], spent := 0
  , observedTerminal := 0, consecutiveFailures := 0, state := .running }

/-- The adopted job row `own` with its one live child in slot `s`. -/
private def ownJob : Job :=
  { stepZero "own" with
    pin := some (hostPin guestPin facts)
  , children := [{ key := { root := "own", slot := "s", attempt := 0 }, target := worker
                 , state := .live, messages := 0 }]
  , spent := 1 }

private def rowStateWire : RowState → String
  | .running => "running"
  | .completed => "completed"
  | .failed => "failed"
  | .cancelled => "cancelled"
  | .timedOut => "timedOut"

private def noExtras : Extras := { writes := 0, scratch := [], progress := false }

private def observation : SpawnKey → Recovery.KillObservation := fun _ => .causedLiveLocal

structure ControlEntry where
  target : String
  kind : String
  deriving ToJson

/-- A pinless step-0 answer: `writes` inserts into the plugin's data grant and
`control` entries naming `own` or `foreign`. A non-empty `control` is answered
while `own` runs. -/
structure Step0Answer where
  writes : Nat
  control : List ControlEntry
  deriving ToJson

/-- The answer the model reads: the guest's own result, or the host's refusal. -/
structure Step0Observed where
  answered : String
  code : Option String
  failureClass : Option String
  state : String
  awaitMode : String
  pinned : Bool
  receipt : Bool
  committedWrites : Nat
  ownJobState : Option String
  ownChildInterrupted : Option Bool
  deriving ToJson

private def kindOf (kind : String) : ControlKind :=
  if kind == "pause" then .pause else .stop

private def answerOf (a : Step0Answer) : StepOutput :=
  .result { noExtras with writes := a.writes } (a.control.map fun e => (e.target, kindOf e.kind))
    none none

private def entry (target kind : String) : ControlEntry := { target, kind }

@[conformance_family]
def nonJob : Conformance.Family Step0Answer Step0Observed where
  inputs :=
    [ { writes := 1, control := [] }
    , { writes := 0, control := [] }
    , { writes := 0, control := [entry "own" "stop"] }
    , { writes := 0, control := [entry "own" "pause"] }
    , { writes := 0, control := [entry "own" "stop", entry "foreign" "stop"] }
    , { writes := 0, control := [entry "foreign" "stop"] } ]
  model := fun a =>
    let out := answerOf a
    let row := callingRow context (stepZero "call") out
    let own := controlled (controlKills context (stepZero "call") out) "own" ownJob observation
    let code := refusalCode context (stepZero "call") out
    { answered := if code.isSome then "refusal" else "result"
    , code
    , failureClass := failureClass context (stepZero "call") out
    , state := rowStateWire row.state
    , awaitMode := if row.background then "background" else "foreground"
    , pinned := row.pinned
    , receipt := row.receipt
    , committedWrites := committedWrites context (stepZero "call") out
    , ownJobState := if a.control.isEmpty then none else some (rowStateWire own.1.state)
    , ownChildInterrupted := if a.control.isEmpty then none else some (!own.2.isEmpty) }

theorem non_job_result_with_writes_commits_atomically_with_the_foreground_terminal :
    callingRow context (stepZero "call") (.result { noExtras with writes := 1 } [] none none) =
        { state := .completed, background := false, pinned := false, receipt := true } ∧
      committedWrites context (stepZero "call") (.result { noExtras with writes := 1 } [] none none) =
        1 := by native_decide

theorem calling_row_of_a_control_answer_is_never_adopted :
    callingRow context (stepZero "call") (.result noExtras [("own", .stop)] none none) =
      { state := .completed, background := false, pinned := false, receipt := true } := by
  native_decide

theorem an_unauthorized_later_entry_kills_nothing :
    let out : StepOutput := .result noExtras [("own", .stop), ("foreign", .stop)] none none
    refusalCode context (stepZero "call") out = some "plugin_job_policy_denied" ∧
      controlKills context (stepZero "call") out = [] ∧
      (controlled (controlKills context (stepZero "call") out) "own" ownJob observation).1.state =
        .running := by native_decide

/-- Step-0 adoptions in one session, each answering with the same guest
handle and one scratch file. -/
structure Adoptions where
  callIds : List String
  handle : String
  scratchName : String
  scratchContent : String
  deriving ToJson

structure ScratchFile where
  path : String
  content : String
  deriving ToJson

structure Adopted where
  handles : List String
  scratchFiles : List ScratchFile
  handleDirs : List String
  deriving ToJson

private def handlesOf (handle : String) (calls : List String) : List String :=
  calls.foldl (fun taken _ => taken ++ [uniqueHandle taken handle]) []

@[conformance_family]
def adoptions : Conformance.Family Adoptions Adopted where
  inputs :=
    [["call-1"], ["call-1", "call-2"], ["call-1", "call-2", "call-3"]].map fun callIds =>
      { callIds, handle := "h", scratchName := "a.txt", scratchContent := "x" }
  model := fun a =>
    let handles := handlesOf a.handle a.callIds
    { handles
    , scratchFiles := a.callIds.filterMap fun call =>
        (scratchAdmitted "." (fun _ => true) call a.scratchName a.scratchContent.length 1).map
          fun path => { path, content := a.scratchContent }
    , handleDirs := handles.filter (· ∈ a.callIds) }

theorem duplicate_handle_is_suffixed_and_the_scratch_path_is_unchanged :
    handlesOf "h" ["call-1", "call-2"] = ["h", "h-2"] ∧
      scratchAdmitted "." (fun _ => true) "call-2" "a.txt" 1 1 =
        some "./.gents/workflows/runs/call-2/a.txt" := by native_decide

/-- A later step of the adopted job `own`, whose one child in slot `s` is
live, answering `spawn` (one spawn of `worker` in slot `later`) or nothing.
The runtime reads its Node's hop cap: `capReadable` with `cap`, or a Node it
cannot read. The Agent's own view of the Node plays no part. -/
structure LaterStep where
  capReadable : Bool
  cap : Option Nat
  spawn : Bool
  deriving ToJson

structure LaterObserved where
  code : Option String
  rowState : String
  spawnedLater : Bool
  deriving ToJson

private def laterOutput (spawn : Bool) : StepOutput :=
  .effects { pin := none, resume := none
           , spawns := if spawn then [{ slot := "later", attempt := 0, target := worker, cwd := none }]
                       else []
           , messages := [], interrupts := [], extras := noExtras }

private def laterContext (l : LaterStep) : StepContext :=
  { context with
    maxHop := CausalHop.observedHopBound (if l.capReadable then some l.cap else none) }

@[conformance_family]
def laterStep : Conformance.Family LaterStep LaterObserved where
  inputs :=
    [ { capReadable := true, cap := none, spawn := true }
    , { capReadable := true, cap := some 0, spawn := true }
    , { capReadable := true, cap := some 1, spawn := true }
    , { capReadable := false, cap := none, spawn := true }
    , { capReadable := false, cap := none, spawn := false }
    , { capReadable := true, cap := some 0, spawn := false } ]
  model := fun l =>
    match admitStepGuarded (laterContext l) ownJob (laterOutput l.spawn) with
    | .ok j =>
      { code := none, rowState := rowStateWire j.state
      , spawnedLater := j.children.any (·.key.slot == "later") }
    | .error r =>
      { code := r.code
      , rowState := rowStateWire (settleStepGuarded (laterContext l) ownJob (laterOutput l.spawn)
          observation).1.state
      , spawnedLater := false }

theorem later_step_hop_cap_ignores_agent_node_visibility_and_fails_closed :
    (admitStepGuarded (laterContext ⟨true, none, true⟩) ownJob (laterOutput true)).toOption.isSome ∧
      refusalCode (laterContext ⟨true, some 0, true⟩) ownJob (laterOutput true) =
        some "plugin_job_depth_exceeded" ∧
      refusalCode (laterContext ⟨false, none, true⟩) ownJob (laterOutput true) =
        some "plugin_job_depth_exceeded" ∧
      (admitStepGuarded (laterContext ⟨false, none, false⟩) ownJob (laterOutput false)).toOption.isSome := by
  native_decide

end Conformance.PluginJobDispatch
