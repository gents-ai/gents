import Proofs.PluginJob.Admission
import Proofs.PluginJob.Adoption
import Proofs.PluginJob.Step
import Proofs.PluginJob.Authority
import Proofs.Recovery.Sweeps.PluginJob
import Lean

/-!
# Generated plugin-job admission, adoption, key and control cases

Admission: every `expected` is computed by `admitStep`, `Refusal.code` and
`settleStep`, which fails a refused row closed and runs `Control.kill` over its
own children. Adoption: by `admitStep`, `adopt`, `adoptTerminal`, `hostPin`,
`uniqueHandle`, `splitAddress`, the receipt compare-and-set and `resumeValid`.
Keys: by the `SpawnKey` encoders and `decodeRetryKey?`. Control: by
`Control.kill`, `resumeValid`, `resumePin`, `rootCallOf`, `echo`,
`resultOffered`, `resultReadAuthorized` and `onInvocationFailure`. Authority:
by `provenanceOf`, `recordAfterInstall`, `originOf`, `trustedHint`,
`packJobBlockAdmissible`, `Policy.valid` and `wireTargetsFrom`. Wire: protocol
samples built from the `Types` constructors and rendered in the
`gents_protocol::job` spellings, with echoes from `stepEcho`, verified bytes
from `verifiedScript`, mount hints from `trustedHint`, origin from `originOf`
and host codes from `Refusal.code`. Lineage: by `DurableLineage.openingRequest`.
Recovery: by one tick of the registered `pluginJobRecoverySweep`.
-/

open PluginJob ToolPolicy Lean

namespace Conformance.PluginJob

private def targetA : Target := ("node-a", "reviewer")
private def targetB : Target := ("node-b", "writer")
private def targetC : Target := ("node-c", "deployer")
private def knownTargets : List Target := [targetA, targetB, targetC]
private def knownSources : List String := ["source-1", "source-2"]

private def scope (ts : List Target) : EndpointScope Target Unit := .only ts.toFinset fun _ => ()

private def ceiling : Ceiling :=
  { targets := scope [targetA, targetB], maxLive := 4, maxSpawns := 4, maxMessagesPerSlot := 2
  , resultBytes := 4000, maxWallSecs := 7200 }

private def facts : HostFacts :=
  { engineDigest := "engine-1", pluginRef := "job_plugin_fixture/job_fixture", cwd := "/work"
  , argsDigest := "args-1", chainRoot := "row-1", origin := .installed, adoptedAt := 0
  , resumeOf := none, replayedFrom := none, ceilingAtPin := ceiling }

private def guestPin : GuestPin :=
  { scriptId := "flow", sourceDigest := "source-1", handle := "flow-1", agentBudget := 3 }

private def context : StepContext :=
  { live := ceiling, available := true
  , callerTargets := scope [targetA, targetB], jobTargets := scope [targetA, targetB]
  , policy := { mode := .run, dynamicTargets := scope [targetA]
                , trustedPacks := ["job_plugin_fixture"], projectRoots := [] }
  , engineDigest := "engine-1", engineProvenanceOperator := true
  , installedTrusted := fun digest => digest == "source-1"
  , maxHop := some 4, targetHop := fun _ => 0
  , cwdAllowed := fun _ => true, within := fun base cwd => base == cwd
  , liveBackground := 0, startedByJob := false, namedOrigin := none, adoptionFacts := facts
  , callerScope := { requestId := "request-1", sessionId := "session-1", nodeDid := "did:key:node"
                   , requesterDid := none }
  , controlTarget := fun _ => none, resumeTarget := fun _ => none
  , resumeIsHead := fun _ => false, resumeFacts := facts }

private def child (slot : String) (state : ChildState) : Child :=
  { key := { root := "row-1", slot, attempt := 0 }, target := targetA, state, messages := 0 }

private def stepZero : Job :=
  { root := "row-1", rowDoc := "row-1", callerSession := "session-1", callerCwd := "/work"
  , handle := "flow-1", callerHop := 0
  , call := { argv := ["run", "flow"], args := none, path := none, script := none
            , agentBudget := none }
  , rootCall := none, resume := none, pin := none, children := [], spent := 0
  , observedTerminal := 0, consecutiveFailures := 0, state := .running }

private def pinned (children : List Child) (spent observed : Nat) : Job :=
  { stepZero with pin := some (hostPin guestPin facts), children, spent
                , observedTerminal := observed }

private def spawn (slot : String) (target : Target) : Spawn :=
  { slot, attempt := 0, target, cwd := none }

private def noExtras : Extras := { writes := 0, scratch := [], progress := false }

private def effects (spawns : List Spawn) (pin : Option GuestPin := none) : StepOutput :=
  .effects { pin, resume := none, spawns, messages := [], interrupts := [], extras := noExtras }

structure AdmissionCase where
  name : String
  ctx : StepContext
  job : Job
  out : StepOutput

/-- A completed, pinned prior row `row-0` of the caller's session, the chain
head only when `head`. -/
private def resumeContext (head : Bool) : StepContext :=
  let prior : Job := { pinned [] 0 0 with root := "row-0", rowDoc := "row-0", state := .completed }
  { context with
    resumeTarget := fun d =>
      if d == "row-0" then some (context.callerScope, true, prior, prior) else none
  , resumeIsHead := fun d => head && d == "row-0"
  , resumeFacts := { facts with resumeOf := some "row-0" } }

private def resumeOf (row : DocId) : StepOutput :=
  .suspend .pause none noExtras (some guestPin)
    (some { rowDoc := row, engineCurrent := false, answer := none, budget := none })

def admissionCases : List AdmissionCase :=
  [ ⟨"panel_one_refused_target_admits_none", context, pinned [] 0 0,
      effects [spawn "1:a" targetA, spawn "2:c" targetC]⟩
  , ⟨"reserved_keys_charge_nothing", context, pinned [child "1:a" .live] 1 0,
      effects [spawn "1:a" targetA]⟩
  , ⟨"budget_exceeded_suspend_live_zero_admitted", context,
      pinned [child "1:a" .completed, child "2:b" .completed, child "3:c" .completed] 3 3,
      .suspend .pause (some .budgetExceeded) noExtras none none⟩
  , ⟨"over_budget_panel_policy_denied_and_killed", context,
      pinned [child "1:a" .live, child "2:b" .completed, child "3:c" .completed] 3 2,
      effects [spawn "4:d" targetA]⟩
  , ⟨"terminal_with_live_children_refused", context, pinned [child "1:a" .live] 1 0,
      .result noExtras [] none none⟩
  , ⟨"resume_without_pin_refused", context, stepZero,
      .result noExtras [] none
        (some { rowDoc := "row-0", engineCurrent := false, answer := none, budget := none })⟩
  , ⟨"operator_narrowing_refuses_removed_target",
      { context with live := { ceiling with targets := scope [targetA] } },
      pinned [child "1:a" .live] 1 0, effects [spawn "2:b" targetB]⟩
  , ⟨"started_by_job_refused", { context with startedByJob := true }, stepZero,
      effects [spawn "1:a" targetA] (some guestPin)⟩
  , ⟨"dynamic_wider_target_refused", context,
      { pinned [] 0 0 with pin := some (hostPin guestPin { facts with origin := .dynamic }) },
      effects [spawn "1:b" targetB]⟩
  , ⟨"stalled_step_refused", context, pinned [] 0 0, effects []⟩
  , ⟨"full_background_budget_returns_capacity_refusal",
      { context with liveBackground := Background.maxBackgroundedPerParent }, stepZero,
      effects [] (some guestPin)⟩
  , ⟨"absent_hop_cap_admits_deep_target",
      { context with maxHop := none, targetHop := fun target => if target == targetA then 100 else 0 },
      stepZero, effects [spawn "1:a" targetA] (some guestPin)⟩
  , ⟨"scratch_then_complete_admitted", context, pinned [] 0 0,
      .result { writes := 1, scratch := ["runs/call-1/report.md"], progress := false } [] none
        none⟩
  , ⟨"resume_of_chain_head_admitted", resumeContext true, stepZero, resumeOf "row-0"⟩
  , ⟨"resume_of_unknown_row_refused", resumeContext true, stepZero, resumeOf "row-9"⟩
  , ⟨"resume_of_continued_row_refused", resumeContext false, stepZero, resumeOf "row-0"⟩ ]


private def observation : SpawnKey → Recovery.KillObservation := fun _ => .causedLiveLocal

private def settled (c : AdmissionCase) : Job × List (SpawnKey × Recovery.KillAction) :=
  settleStep c.ctx c.job c.out observation

/-- The refusal code, the admitted row's state, spend and child count, and the
number of kill actions. -/
private def verdict (c : AdmissionCase) : Option String × Option (RowState × Nat × Nat) × Nat :=
  match admitStep c.ctx c.job c.out with
  | .ok j => (none, some (j.state, j.spent, j.children.length), (settled c).2.length)
  | .error r => (r.code, none, (settled c).2.length)

theorem admission_regressions :
    admissionCases.map verdict =
      [ (some "plugin_job_policy_denied", none, 0)
      , (none, some (.running, 1, 1), 0)
      , (none, some (.completed, 3, 3), 0)
      , (some "plugin_job_policy_denied", none, 1)
      , (some "plugin_job_policy_denied", none, 1)
      , (some "plugin_job_policy_denied", none, 0)
      , (some "plugin_job_policy_denied", none, 1)
      , (some "plugin_job_depth_exceeded", none, 0)
      , (some "plugin_job_policy_denied", none, 0)
      , (some "plugin_job_stalled", none, 0)
      , (some "background_tool_budget_exceeded", none, 0)
      , (none, some (.running, 1, 1), 0)
      , (none, some (.completed, 0, 0), 0)
      , (none, some (.completed, 0, 0), 0)
      , (some "plugin_job_resume_unknown", none, 0)
      , (some "plugin_job_resume_not_head", none, 0) ] := by native_decide

private def targetJson (t : Target) : Json :=
  Json.mkObj [("node", toJson t.1), ("agent", toJson t.2)]

private def scopeJson : EndpointScope Target Unit → Json
  | .none => Json.mkObj [("kind", toJson "none")]
  | .all => Json.mkObj [("kind", toJson "all")]
  | sc@(.only _ _) => Json.mkObj [("kind", toJson "only"),
      ("targets", toJson ((knownTargets.filter fun t => decide (sc.permits t)).map targetJson))]

private def ceilingJson (c : Ceiling) : Json :=
  Json.mkObj [("targets", scopeJson c.targets), ("max_live", toJson c.maxLive),
    ("max_spawns", toJson c.maxSpawns), ("max_messages_per_slot", toJson c.maxMessagesPerSlot),
    ("result_bytes", toJson c.resultBytes), ("max_wall_secs", toJson c.maxWallSecs)]

private def originJson : Origin → Json
  | .installed => toJson "installed"
  | .dynamic => toJson "dynamic"

private def modeJson : DynamicMode → Json
  | .off => toJson "off"
  | .check => toJson "check"
  | .run => toJson "run"

private def rowStateJson : RowState → Json
  | .running => toJson "running"
  | .completed => toJson "completed"
  | .failed => toJson "failed"
  | .cancelled => toJson "cancelled"
  | .timedOut => toJson "timed_out"

private def childStateJson : ChildState → Json
  | .live => toJson "live"
  | .completed => toJson "completed"
  | .failed => toJson "failed"
  | .interrupted => toJson "interrupted"
  | .other => toJson "other"

private def pauseReasonJson : PauseReason → Json
  | .user => toJson "user"
  | .backOff => toJson "back_off"
  | .noProgress => toJson "no_progress"
  | .verification => toJson "verification"
  | .infra => toJson "infra"
  | .budgetExceeded => toJson "budget_exceeded"

private def hostFactsJson (f : HostFacts) : Json :=
  Json.mkObj [("engine_digest", toJson f.engineDigest), ("plugin_ref", toJson f.pluginRef),
    ("cwd", toJson f.cwd), ("args_digest", toJson f.argsDigest),
    ("chain_root", toJson f.chainRoot), ("origin", originJson f.origin),
    ("adopted_at", toJson f.adoptedAt), ("resume_of", toJson f.resumeOf),
    ("replayed_from", toJson f.replayedFrom), ("ceiling_at_pin", ceilingJson f.ceilingAtPin)]

private def pinJson (p : Pin) : Json :=
  Json.mkObj [("script_id", toJson p.scriptId), ("source_digest", toJson p.sourceDigest),
    ("engine_digest", toJson p.engineDigest), ("plugin_ref", toJson p.pluginRef),
    ("handle", toJson p.handle), ("cwd", toJson p.cwd), ("args_digest", toJson p.argsDigest),
    ("chain_root", toJson p.chainRoot), ("origin", originJson p.origin),
    ("agent_budget", toJson p.agentBudget), ("adopted_at", toJson p.adoptedAt),
    ("resume_of", toJson p.resumeOf), ("replayed_from", toJson p.replayedFrom),
    ("ceiling_at_pin", ceilingJson p.ceilingAtPin)]

private def guestPinJson (g : GuestPin) : Json :=
  Json.mkObj [("script_id", toJson g.scriptId), ("source_digest", toJson g.sourceDigest),
    ("handle", toJson g.handle), ("agent_budget", toJson g.agentBudget)]

private def resumeJson (r : Resume) : Json :=
  Json.mkObj [("row_doc_id", toJson r.rowDoc), ("engine_current", toJson r.engineCurrent),
    ("answer", toJson r.answer), ("budget", toJson r.budget)]

private def childJson (ch : Child) : Json :=
  Json.mkObj [("retry_key", toJson ch.key.retryKey), ("slot", toJson ch.key.slot),
    ("attempt", toJson ch.key.attempt), ("target", targetJson ch.target),
    ("state", childStateJson ch.state), ("messages", toJson ch.messages)]

private def callArgsJson (c : CallArgs) : Json :=
  Json.mkObj [("argv", toJson c.argv), ("args", toJson c.args), ("path", toJson c.path),
    ("script", toJson c.script), ("agent_budget", toJson c.agentBudget)]

private def rootCallJson (c : RootCall) : Json :=
  Json.mkObj [("argv", toJson c.argv), ("args", toJson c.args), ("path", toJson c.path),
    ("agent_budget", toJson c.agentBudget)]

private def jobJson (j : Job) : Json :=
  Json.mkObj [("root", toJson j.root), ("row_doc_id", toJson j.rowDoc),
    ("caller_session", toJson j.callerSession), ("caller_cwd", toJson j.callerCwd),
    ("handle", toJson j.handle), ("caller_hop", toJson j.callerHop),
    ("call", callArgsJson j.call), ("root_call", toJson (j.rootCall.map rootCallJson)),
    ("resume", toJson (j.resume.map resumeJson)), ("pin", toJson (j.pin.map pinJson)),
    ("children", toJson (j.children.map childJson)), ("spent", toJson j.spent),
    ("observed_terminal", toJson j.observedTerminal),
    ("consecutive_failures", toJson j.consecutiveFailures), ("state", rowStateJson j.state)]

private def processScopeJson (s : Background.ProcessControl.Scope) : Json :=
  Json.mkObj [("request_id", toJson s.requestId), ("session_id", toJson s.sessionId),
    ("node_did", toJson s.nodeDid), ("requester_did", toJson s.requesterDid)]

private def extrasJson (x : Extras) : Json :=
  Json.mkObj [("writes", toJson x.writes), ("scratch", toJson x.scratch),
    ("progress", toJson x.progress)]

private def spawnJson (s : Spawn) : Json :=
  Json.mkObj [("slot", toJson s.slot), ("attempt", toJson s.attempt),
    ("target", targetJson s.target), ("cwd", toJson s.cwd)]

private def outputJson : StepOutput → Json
  | .result x control pin resume => Json.mkObj [("kind", toJson "result"),
      ("extras", extrasJson x),
      ("control", toJson (control.map fun (row, k) => Json.mkObj [("row_doc_id", toJson row),
        ("control", toJson (match k with | .stop => "stop" | .pause => "pause"))])),
      ("pin", toJson (pin.map guestPinJson)), ("resume", toJson (resume.map resumeJson))]
  | .fail x => Json.mkObj [("kind", toJson "fail"), ("extras", extrasJson x)]
  | .suspend k reason x pin resume => Json.mkObj [("kind", toJson "suspend"),
      ("suspend", toJson (match k with | .pause => "pause" | .awaitUser => "await_user")),
      ("reason", toJson (reason.map pauseReasonJson)), ("extras", extrasJson x),
      ("pin", toJson (pin.map guestPinJson)), ("resume", toJson (resume.map resumeJson))]
  | .effects e => Json.mkObj [("kind", toJson "effects"),
      ("pin", toJson (e.pin.map guestPinJson)), ("resume", toJson (e.resume.map resumeJson)),
      ("spawns", toJson (e.spawns.map spawnJson)), ("messages", toJson e.messages),
      ("interrupts", toJson e.interrupts), ("extras", extrasJson e.extras)]

/-- The rows and working directories the function-valued context fields are
tabulated over: every document id and cwd a case names or probes. -/
private def knownDocs : List DocId := ["row-0", "row-1", "row-a", "row-b"]
private def knownCwds : List String := ["/", "/work", "/work/sub"]

private def policyJson (p : PluginJob.Policy) : Json :=
  Json.mkObj [("mode", modeJson p.mode), ("dynamic_targets", scopeJson p.dynamicTargets),
    ("trusted_packs", toJson p.trustedPacks), ("project_roots", toJson p.projectRoots)]

private def contextJson (ctx : StepContext) : Json :=
  Json.mkObj [("live", ceilingJson ctx.live), ("available", toJson ctx.available),
    ("caller_targets", scopeJson ctx.callerTargets), ("job_targets", scopeJson ctx.jobTargets),
    ("policy", policyJson ctx.policy),
    ("engine_digest", toJson ctx.engineDigest),
    ("engine_provenance_operator", toJson ctx.engineProvenanceOperator),
    ("installed_trusted_sources", toJson (knownSources.filter ctx.installedTrusted)),
    ("max_hop", toJson ctx.maxHop),
    ("target_hops", toJson (knownTargets.map fun t =>
      Json.mkObj [("target", targetJson t), ("hop", toJson (ctx.targetHop t))])),
    ("live_background", toJson ctx.liveBackground), ("started_by_job", toJson ctx.startedByJob),
    ("cwd_allowed", toJson (knownCwds.filter ctx.cwdAllowed)),
    ("within", toJson ((knownCwds.flatMap fun base => knownCwds.filterMap fun cwd =>
      if ctx.within base cwd then some (Json.mkObj [("base", toJson base), ("cwd", toJson cwd)])
      else none))),
    ("named_origin", toJson (ctx.namedOrigin.map originJson)),
    ("adoption_facts", hostFactsJson ctx.adoptionFacts),
    ("caller_scope", processScopeJson ctx.callerScope),
    ("control_targets", toJson (knownDocs.map fun d =>
      Json.mkObj [("row_doc_id", toJson d),
        ("target", toJson ((ctx.controlTarget d).map fun (owner, samePlugin) => Json.mkObj
          [("owner", processScopeJson owner), ("same_plugin", toJson samePlugin)]))])),
    ("resume_targets", toJson (knownDocs.map fun d =>
      Json.mkObj [("row_doc_id", toJson d), ("is_head", toJson (ctx.resumeIsHead d)),
        ("target", toJson ((ctx.resumeTarget d).map fun (owner, samePlugin, prior, root) =>
          Json.mkObj [("owner", processScopeJson owner), ("same_plugin", toJson samePlugin),
            ("prior", jobJson prior), ("root", jobJson root)]))])),
    ("resume_facts", hostFactsJson ctx.resumeFacts)]

private def render (c : AdmissionCase) : Json :=
  let (row, kill) := settled c
  let (code, admitted) := match admitStep c.ctx c.job c.out with
    | .ok j => ((none : Option String), some j)
    | .error r => (r.code, none)
  Json.mkObj [("name", toJson c.name), ("context", contextJson c.ctx), ("job", jobJson c.job),
    ("output", outputJson c.out),
    ("expected", Json.mkObj [("admitted", toJson admitted.isSome), ("code", toJson code),
      ("job", toJson (admitted.map jobJson)), ("row_state", rowStateJson row.state),
      ("kill", toJson (kill.map fun (k, a) =>
        Json.mkObj [("retry_key", toJson k.retryKey), ("action", toJson a.toContract)]))])]

def admissionCasesJson : String := (toJson (admissionCases.map render)).compress

private def toolCall : ToolExecution.ToolCallContext :=
  { callId := 1, requestId := 1, state := .running, operation := .nativeCommand, deadline := 20
  , startedAt := some 1, currentTime := 5, persistence := .committed, awaitMode := .foreground }

private def chainFacts : HostFacts := { facts with chainRoot := "row-a", argsDigest := "args-a" }

/-- Row A: suspended on `await_user`, pinned, and the chain head. -/
private def rowA : Job :=
  { stepZero with root := "row-a", rowDoc := "row-a", handle := "flow-1"
                , pin := some (hostPin guestPin chainFacts), state := .completed }

/-- Row B: the `workflow answer` call, with its own handle and call arguments. -/
private def rowB : Job :=
  { stepZero with root := "row-b", rowDoc := "row-b", handle := "flow-b"
                , call := { stepZero.call with argv := ["answer", "flow-1", "yes"] } }

private def answerContext : StepContext :=
  { context with
    resumeTarget := fun d =>
      if d == "row-a" then some (context.callerScope, true, rowA, rowA) else none
  , resumeIsHead := fun d => d == "row-a"
  , resumeFacts :=
      { facts with resumeOf := some "row-a", chainRoot := "row-b", argsDigest := "args-b" } }

private def answerResume : Resume :=
  { rowDoc := "row-a", engineCurrent := false, answer := some "yes", budget := none }

structure Step0Case where
  name : String
  row : ToolExecution.ToolCallContext
  ctx : StepContext
  job : Job
  out : StepOutput

def step0Cases : List Step0Case :=
  [ ⟨"step0_effects_adopted_single_receipt", toolCall, context, stepZero,
      effects [spawn "1:a" targetA] (some guestPin)⟩
  , ⟨"step0_pinned_result_delivered_inline", toolCall, context, stepZero,
      .result noExtras [] (some guestPin) none⟩
  , ⟨"step0_pinned_suspend_delivered_inline", toolCall, context, stepZero,
      .suspend .pause (some .budgetExceeded) noExtras (some guestPin) none⟩
  , ⟨"step0_pinned_result_with_scratch", toolCall, context, stepZero,
      .result { writes := 1, scratch := ["runs/call-1/report.md"], progress := false } []
        (some guestPin) none⟩
  , ⟨"two_consecutive_awaits_terminal_chain_row", toolCall, answerContext, rowB,
      .suspend .awaitUser none noExtras (some { guestPin with handle := "flow-2" })
        (some answerResume)⟩ ]

private def lateReceipt : PluginReceipt.Receipt :=
  { evidence := "late", output := none, verdict := .failed }

private def finalReceipt : PluginReceipt.Receipt :=
  { evidence := "final", output := some "done", verdict := .success }

/-- The adopted row carries no receipt, so its terminal compare-and-set
commits; a later terminal attempt against the receipt it wrote is refused. -/
private def receiptDecisions : PluginReceipt.Decision × PluginReceipt.Decision :=
  let row := PluginReceipt.step ⟨false, none⟩ .completed finalReceipt
  (PluginReceipt.decide false none (PluginReceipt.published .completed finalReceipt),
    PluginReceipt.decide row.terminal row.receipt (PluginReceipt.published .failed lateReceipt))

/-- The next `answer` naming the admitted row as its prior. -/
private def nextResumeValid (c : Step0Case) (j : Job) : Bool :=
  resumeValid c.ctx.callerScope c.ctx.callerScope true j guestPin
    { c.ctx.adoptionFacts with resumeOf := some j.rowDoc }
    { rowDoc := j.rowDoc, engineCurrent := false, answer := some "next", budget := none }

private def adoptedCall (c : Step0Case) :
    ToolExecution.ToolCallContext × Option Delivery :=
  match admitStep c.ctx c.job c.out, c.out.pin with
  | .ok _, some g =>
    match c.out with
    | .effects e => ((adopt c.row (adoptedPin c.ctx c.out g) e).1, none)
    | out =>
      let (row, d) := adoptTerminal c.row (adoptedPin c.ctx c.out g) out
      (row, some d)
  | _, _ => (c.row, none)

structure Step0Verdict where
  code : Option String
  state : Option RowState
  /-- Handle, chain root, `args_digest` and `resume_of` of the written pin. -/
  pin : Option (String × String × String × Option DocId)
  call : ToolExecution.ToolCallState × ToolExecution.ToolOperation × ToolExecution.AwaitMode
  delivery : Option Delivery
  receipt : Option (PluginReceipt.Decision × PluginReceipt.Decision)
  nextResume : Bool
  deriving DecidableEq

private def step0Verdict (c : Step0Case) : Step0Verdict :=
  let (call, delivery) := adoptedCall c
  match admitStep c.ctx c.job c.out with
  | .ok j =>
    { code := none, state := some j.state
    , pin := j.pin.map fun p => (p.handle, p.chainRoot, p.argsDigest, p.resumeOf)
    , call := (call.state, call.operation, call.awaitMode), delivery := delivery
    , receipt := some receiptDecisions, nextResume := nextResumeValid c j }
  | .error r =>
    { code := r.code, state := none, pin := none
    , call := (call.state, call.operation, call.awaitMode), delivery := delivery
    , receipt := none, nextResume := false }

/-- A guest-written pin claiming every field; only the `GuestPin` fields reach
`hostPin`. -/
structure HostPinCase where
  name : String
  claimed : Pin
  facts : HostFacts

private def guestOf (p : Pin) : GuestPin :=
  { scriptId := p.scriptId, sourceDigest := p.sourceDigest, handle := p.handle
  , agentBudget := p.agentBudget }

def hostPinCases : List HostPinCase :=
  [ ⟨"guest_pin_host_fields_overwritten",
      { scriptId := "flow", sourceDigest := "source-2", engineDigest := "guest-engine"
      , pluginRef := "forged/plugin", handle := "flow-9", cwd := "/", argsDigest := "forged-args"
      , chainRoot := "forged-root", origin := .installed, agentBudget := 5, adoptedAt := 99
      , resumeOf := some "forged-prior", replayedFrom := some "forged-replay"
      , ceilingAtPin := { ceiling with maxLive := 64 } },
      { facts with origin := .dynamic }⟩ ]

/-- The written pin's string, origin, number and optional row fields. -/
private def hostPinVerdict (c : HostPinCase) :
    List String × Origin × List Nat × List (Option DocId) :=
  let p := hostPin (guestOf c.claimed) c.facts
  ([p.scriptId, p.sourceDigest, p.handle, p.engineDigest, p.pluginRef, p.cwd, p.argsDigest,
      p.chainRoot], p.origin, [p.agentBudget, p.adoptedAt, p.ceilingAtPin.maxLive],
    [p.resumeOf, p.replayedFrom])

structure HandleCase where
  name : String
  taken : List String
  handle : String

def handleCases : List HandleCase :=
  [ ⟨"free_handle_kept", ["review"], "flow"⟩
  , ⟨"duplicate_handle_suffixed", ["flow"], "flow"⟩
  , ⟨"duplicate_handle_skips_taken_suffix", ["flow", "flow-2"], "flow"⟩ ]

structure AddressCase where
  name : String
  address : String

private def childSession : String :=
  ({ root := "row-1", slot := "1:a", attempt := 0 } : SpawnKey).sessionId

def addressCases : List AddressCase :=
  [ ⟨"address_session_with_colons", childSession ++ ":" ++ "call-7"⟩
  , ⟨"address_plain_session", "session-1:call-1"⟩
  , ⟨"address_without_colon", "call-1"⟩ ]

theorem adoption_regressions :
    step0Cases.map step0Verdict =
      [ ⟨none, some .running, some ("flow-1", "row-1", "args-1", none),
          (.running, .pluginJob, .background), none, some (.commit, .conflict), false⟩
      , ⟨none, some .completed, some ("flow-1", "row-1", "args-1", none),
          (.completed, .pluginJob, .background), some .inline, some (.commit, .conflict), true⟩
      , ⟨none, some .completed, some ("flow-1", "row-1", "args-1", none),
          (.completed, .pluginJob, .background), some .inline, some (.commit, .conflict), true⟩
      , ⟨none, some .completed, some ("flow-1", "row-1", "args-1", none),
          (.completed, .pluginJob, .background), some .inline, some (.commit, .conflict), true⟩
      , ⟨none, some .completed, some ("flow-1", "row-a", "args-a", some "row-a"),
          (.completed, .pluginJob, .background), some .inline, some (.commit, .conflict),
          true⟩ ] ∧
    hostPinCases.map hostPinVerdict =
      [ (["flow", "source-2", "flow-9", "engine-1", "job_plugin_fixture/job_fixture", "/work",
          "args-1", "row-1"], .dynamic, [5, 0, 4], [none, none]) ] ∧
    handleCases.map (fun c => uniqueHandle c.taken c.handle) = ["flow", "flow-2", "flow-3"] ∧
    addressCases.map (fun c => splitAddress c.address) =
      [ some ("job-session:5:row-13:1:a1:0", "call-7"), some ("session-1", "call-1"), none ] := by
  native_decide

private def deliveryJson : Option Delivery → Json
  | some .inline => toJson "inline"
  | some .continuation => toJson "continuation"
  | none => Json.null

private def callJson (c : ToolExecution.ToolCallContext) : Json :=
  Json.mkObj [("state", toJson c.state.toDefraDB), ("operation", toJson c.operation.toDefraDB),
    ("await_mode", toJson c.awaitMode.toDefraDB)]

private def renderStep0 (c : Step0Case) : Json :=
  let (call, delivery) := adoptedCall c
  let (first, late) := receiptDecisions
  let (code, admitted) := match admitStep c.ctx c.job c.out with
    | .ok j => ((none : Option String), some j)
    | .error r => (r.code, none)
  Json.mkObj [("name", toJson c.name), ("kind", toJson "step0"), ("tool_call", callJson c.row),
    ("context", contextJson c.ctx), ("job", jobJson c.job),
    ("output", outputJson c.out),
    ("expected", Json.mkObj [("admitted", toJson admitted.isSome), ("code", toJson code),
      ("job", toJson (admitted.map jobJson)), ("tool_call", callJson call),
      ("delivery", deliveryJson delivery),
      ("continuation_appended", toJson (delivery == some .continuation)),
      ("receipt", toJson (admitted.map fun _ => Json.mkObj
        [("terminal_decision", toJson first.spelling), ("late_decision", toJson late.spelling)])),
      ("next_resume_valid", toJson (admitted.any (nextResumeValid c)))])]

private def renderHostPin (c : HostPinCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("kind", toJson "host_pin"),
    ("claimed", pinJson c.claimed), ("facts", hostFactsJson c.facts),
    ("expected", Json.mkObj [("pin", pinJson (hostPin (guestOf c.claimed) c.facts))])]

private def renderHandle (c : HandleCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("kind", toJson "handle"), ("taken", toJson c.taken),
    ("handle", toJson c.handle),
    ("expected", Json.mkObj [("handle", toJson (uniqueHandle c.taken c.handle))])]

private def renderAddress (c : AddressCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("kind", toJson "address"), ("address", toJson c.address),
    ("expected", Json.mkObj [("split", toJson ((splitAddress c.address).map fun (s, t) =>
      Json.mkObj [("session_id", toJson s), ("tool_call_id", toJson t)]))])]

def adoptionCasesJson : String :=
  (toJson (step0Cases.map renderStep0 ++ hostPinCases.map renderHostPin ++
    handleCases.map renderHandle ++ addressCases.map renderAddress)).compress

structure KeyCase where
  name : String
  key : SpawnKey
  messages : List Nat

def keyCases : List KeyCase :=
  [ ⟨"ascii_root_and_slot", { root := "row-1", slot := "slot-a", attempt := 0 }, [0, 1]⟩
  , ⟨"slot_with_colons", { root := "row-1", slot := "1:abc", attempt := 2 }, [1, 12]⟩
  , ⟨"root_with_colons", { root := "job-session:5:row-1", slot := "2:def", attempt := 10 }, [3]⟩
  , ⟨"multibyte_root_and_slot", { root := "ルート-1", slot := "3:é✓", attempt := 1 }, [0]⟩
  , ⟨"four_byte_slot", { root := "row-1", slot := "4:🙂", attempt := 7 }, [2]⟩ ]

/-- Retry key, session id, message keys, the retry key decoded, and whether
any message key decodes as a retry key. -/
private def keyVerdict (c : KeyCase) :
    String × String × List String × Option SpawnKey × Bool :=
  (c.key.retryKey, c.key.sessionId, c.messages.map c.key.messageKey, decodeRetryKey? c.key.retryKey,
    c.messages.any fun n => (decodeRetryKey? (c.key.messageKey n)).isSome)

theorem key_regressions :
    keyCases.map keyVerdict =
      [ ("job:5:row-16:slot-a1:0", "job-session:5:row-16:slot-a1:0",
          ["job:5:row-16:slot-a1:01:m1:0", "job:5:row-16:slot-a1:01:m1:1"],
          some { root := "row-1", slot := "slot-a", attempt := 0 }, false)
      , ("job:5:row-15:1:abc1:2", "job-session:5:row-15:1:abc1:2",
          ["job:5:row-15:1:abc1:21:m1:1", "job:5:row-15:1:abc1:21:m2:12"],
          some { root := "row-1", slot := "1:abc", attempt := 2 }, false)
      , ("job:19:job-session:5:row-15:2:def2:10", "job-session:19:job-session:5:row-15:2:def2:10",
          ["job:19:job-session:5:row-15:2:def2:101:m1:3"],
          some { root := "job-session:5:row-1", slot := "2:def", attempt := 10 }, false)
      , ("job:5:ルート-14:3:é✓1:1", "job-session:5:ルート-14:3:é✓1:1",
          ["job:5:ルート-14:3:é✓1:11:m1:0"],
          some { root := "ルート-1", slot := "3:é✓", attempt := 1 }, false)
      , ("job:5:row-13:4:🙂1:7", "job-session:5:row-13:4:🙂1:7",
          ["job:5:row-13:4:🙂1:71:m1:2"],
          some { root := "row-1", slot := "4:🙂", attempt := 7 }, false) ] := by
  native_decide

private def renderKey (c : KeyCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("root", toJson c.key.root), ("slot", toJson c.key.slot),
    ("attempt", toJson c.key.attempt), ("messages", toJson c.messages),
    ("expected", Json.mkObj [("retry_key", toJson c.key.retryKey),
      ("session_id", toJson c.key.sessionId),
      ("message_keys", toJson (c.messages.map fun n =>
        Json.mkObj [("n", toJson n), ("key", toJson (c.key.messageKey n))])),
      ("decoded", toJson ((decodeRetryKey? c.key.retryKey).map fun k =>
        Json.mkObj [("root", toJson k.root), ("slot", toJson k.slot),
          ("attempt", toJson k.attempt)])),
      ("message_keys_decode", toJson (c.messages.any fun n =>
        (decodeRetryKey? (c.key.messageKey n)).isSome))])]

def keyCasesJson : String := (toJson (keyCases.map renderKey)).compress

/-- A child another chain caused; no kill of this row reaches it. -/
private def foreignChild : Child :=
  { child "1:x" .live with key := { root := "row-0", slot := "1:x", attempt := 0 } }

/-- A running row with a local and a remote live own child, a completed own
child and a live foreign child. -/
private def killJob : Job :=
  pinned [child "1:a" .live, child "2:b" .live, child "3:c" .completed, foreignChild] 3 1

private def killObservation (k : SpawnKey) : Recovery.KillObservation :=
  if k.slot == "1:a" then .causedLiveLocal
  else if k.slot == "2:b" then .causedLiveRemote
  else .causedTerminal

structure KillCase where
  name : String
  control : Control
  job : Job := killJob

def killCases : List KillCase :=
  [ { name := "stop_kills_live_own_children", control := .stop }
  , { name := "pause_kills_live_own_children", control := .pause }
  , { name := "deadline_kills_live_own_children", control := .deadline }
  , { name := "stop_loses_to_terminal_driver", control := .stop,
      job := { killJob with state := .failed } }
  , { name := "stop_replay_kills_no_children", control := .stop,
      job := { killJob with state := .cancelled } } ]

private def killedSlots (kill : List (SpawnKey × Recovery.KillAction)) :
    List (String × Recovery.KillAction) :=
  kill.map fun (k, a) => (k.slot, a)

/-- The row state, the job state under the control's note, and the killed
slots with their actions. -/
structure KillVerdict where
  state : RowState
  jobState : JobState
  kills : List (String × Recovery.KillAction)
  deriving DecidableEq

private def killVerdict (c : KillCase) : KillVerdict :=
  let (row, kill) := Control.kill c.job c.control killObservation
  ⟨row.state, JobState.of row.state c.control.note, killedSlots kill⟩

/-- The chain root's pin. -/
private def rootPin : Pin := hostPin guestPin chainFacts

/-- The prior row: an earlier resume of the chain, paused, and the chain head. -/
private def priorJob : Job :=
  { rowA with rowDoc := "row-p", call := { stepZero.call with argv := ["resume", "flow-1"] }
            , pin := some (resumePin rootPin rootPin guestPin
                { chainFacts with argsDigest := "args-p", resumeOf := some "row-a" }) }

private def priorPin : Pin := (priorJob.pin).getD rootPin

/-- The resume verb's own row, with call arguments unlike the chain root's. -/
private def resumeCall : Job :=
  { stepZero with root := "row-r", rowDoc := "row-r", handle := "flow-r"
                , call := { argv := ["resume", "flow-1", "--budget", "5"], args := some "own"
                          , path := none, script := none, agentBudget := some 5 } }

private def resumeFacts : HostFacts :=
  { facts with resumeOf := some "row-p", chainRoot := "row-r", argsDigest := "args-r" }

private def resumeBlock : Resume :=
  { rowDoc := "row-p", engineCurrent := false, answer := none, budget := none }

structure ResumeCase where
  name : String
  guest : GuestPin
  facts : HostFacts
  resume : Resume

def resumeCases : List ResumeCase :=
  [ ⟨"resume_own_args_keep_chain_root", { guestPin with handle := "flow-r" }, resumeFacts,
      resumeBlock⟩
  , ⟨"resume_budget_rises_admitted", { guestPin with agentBudget := 5 }, resumeFacts,
      { resumeBlock with budget := some 5 }⟩
  , ⟨"resume_budget_lowered_refused", { guestPin with agentBudget := 2 }, resumeFacts,
      { resumeBlock with budget := some 2 }⟩
  , ⟨"engine_changed_without_engine_current_refused", guestPin,
      { resumeFacts with engineDigest := "engine-2" }, resumeBlock⟩
  , ⟨"engine_changed_with_engine_current_admitted", guestPin,
      { resumeFacts with engineDigest := "engine-2" }, { resumeBlock with engineCurrent := true }⟩
  , ⟨"answer_echo_precedes_engine_current", guestPin,
      { resumeFacts with engineDigest := "engine-2" },
      { resumeBlock with engineCurrent := true, answer := some "yes" }⟩ ]

private def resumeValidFor (c : ResumeCase) : Bool :=
  resumeValid context.callerScope context.callerScope true priorJob c.guest c.facts c.resume

private def resumePinFor (c : ResumeCase) : Pin := resumePin rootPin priorPin c.guest c.facts

/-- The written pin's chain fields, script, engine, budget and `resume_of`. -/
structure ResumePinView where
  handle : String
  chainRoot : String
  argsDigest : String
  scriptId : String
  engineDigest : String
  agentBudget : Nat
  resumeOf : Option DocId
  deriving DecidableEq

structure ResumeVerdict where
  valid : Bool
  /-- The written pin, when the resume is valid. -/
  pin : Option ResumePinView
  echoKind : String
  rootCall : List String × Option String
  deriving DecidableEq

/-- The echoed control kind and the echoed `root_call`'s argv and args. -/
private def resumeVerdict (c : ResumeCase) : ResumeVerdict :=
  let p := resumePinFor c
  { valid := resumeValidFor c
  , pin := if resumeValidFor c then
      some ⟨p.handle, p.chainRoot, p.argsDigest, p.scriptId, p.engineDigest, p.agentBudget,
        p.resumeOf⟩
    else none
  , echoKind := (echo c.resume).kind
  , rootCall := ((rootCallOf rowA).argv, (rootCallOf rowA).args) }

structure ResultReadCase where
  name : String
  caller : Background.ProcessControl.Scope
  samePlugin : Bool
  terminal : Bool

def resultReadCases : List ResultReadCase :=
  [ ⟨"same_scope_terminal_row_readable", { context.callerScope with requestId := "request-2" },
      true, true⟩
  , ⟨"foreign_session_refused", { context.callerScope with sessionId := "session-2" }, true,
      true⟩
  , ⟨"other_node_refused", { context.callerScope with nodeDid := "did:key:other" }, true, true⟩
  , ⟨"other_requester_refused", { context.callerScope with requesterDid := some "did:key:user" },
      true, true⟩
  , ⟨"other_plugin_refused", context.callerScope, false, true⟩
  , ⟨"running_row_refused", context.callerScope, true, false⟩ ]

private def resultReadVerdict (c : ResultReadCase) : Bool :=
  resultReadAuthorized c.caller context.callerScope c.samePlugin c.terminal

def resultOfferCases : List (String × PluginData.Invocation) :=
  [ ("result_offered_to_tool_call", .toolCall), ("result_offered_to_job_step", .jobStep)
  , ("result_refused_to_callback", .callback) ]

structure StepFailureCase where
  name : String
  failures : Nat

def stepFailureCases : List StepFailureCase :=
  [ ⟨"first_step_failure_counted", 0⟩
  , ⟨"second_step_failure_counted", 1⟩
  , ⟨"third_step_failure_raises_step_failed", 2⟩ ]

private def failingJob (c : StepFailureCase) : Job :=
  { killJob with consecutiveFailures := c.failures }

/-- The row state, the failure count, the killed slots with their actions and
the refusal code. -/
structure StepFailureVerdict where
  state : RowState
  failures : Nat
  kills : List (String × Recovery.KillAction)
  code : Option String
  deriving DecidableEq

private def stepFailureVerdict (c : StepFailureCase) : StepFailureVerdict :=
  let (row, kill, refusal) := Step.onInvocationFailure (failingJob c) killObservation
  ⟨row.state, row.consecutiveFailures, killedSlots kill, refusal.bind Refusal.code⟩

theorem control_regressions :
    killCases.map killVerdict =
      [ ⟨.cancelled, .cancelled, [("1:a", .interruptCaused), ("2:b", .interruptAndCancelRow)]⟩
      , ⟨.completed, .paused, [("1:a", .interruptCaused), ("2:b", .interruptAndCancelRow)]⟩
      , ⟨.timedOut, .timedOut, [("1:a", .interruptCaused), ("2:b", .interruptAndCancelRow)]⟩
      , ⟨.failed, .failed, []⟩
      , ⟨.cancelled, .cancelled, []⟩ ] ∧
    resumeCases.map resumeVerdict =
      [ ⟨true, some ⟨"flow-1", "row-a", "args-a", "flow", "engine-1", 3, some "row-p"⟩, "resume",
          (["run", "flow"], none)⟩
      , ⟨true, some ⟨"flow-1", "row-a", "args-a", "flow", "engine-1", 5, some "row-p"⟩, "resume",
          (["run", "flow"], none)⟩
      , ⟨false, none, "resume", (["run", "flow"], none)⟩
      , ⟨false, none, "resume", (["run", "flow"], none)⟩
      , ⟨true, some ⟨"flow-1", "row-a", "args-a", "flow", "engine-2", 3, some "row-p"⟩,
          "engine_current", (["run", "flow"], none)⟩
      , ⟨true, some ⟨"flow-1", "row-a", "args-a", "flow", "engine-2", 3, some "row-p"⟩, "answer",
          (["run", "flow"], none)⟩ ] ∧
    resultReadCases.map resultReadVerdict = [true, false, false, false, false, false] ∧
    resultOfferCases.map (fun c => resultOffered c.2) = [true, true, false] ∧
    stepFailureCases.map stepFailureVerdict =
      [ ⟨.running, 1, [], none⟩, ⟨.running, 2, [], none⟩
      , ⟨.failed, 3, [("1:a", .interruptCaused), ("2:b", .interruptAndCancelRow)],
          some "plugin_job_step_failed"⟩ ] := by
  native_decide

private def controlJson : Control → Json
  | .stop => toJson "stop"
  | .pause => toJson "pause"
  | .deadline => toJson "deadline"
  | .resume .. => toJson "resume"

private def jobStateJson : JobState → Json
  | .running => toJson "running"
  | .paused => toJson "paused"
  | .awaitingUser => toJson "awaiting_user"
  | .completed => toJson "completed"
  | .failed => toJson "failed"
  | .cancelled => toJson "cancelled"
  | .timedOut => toJson "timed_out"

private def killJson (kill : List (SpawnKey × Recovery.KillAction)) : Json :=
  toJson (kill.map fun (k, a) =>
    Json.mkObj [("retry_key", toJson k.retryKey), ("action", toJson a.toContract)])

private def observationsJson (j : Job) : Json :=
  toJson (j.children.map fun ch => Json.mkObj [("retry_key", toJson ch.key.retryKey),
    ("observation", toJson (killObservation ch.key).toContract)])

private def renderKill (c : KillCase) : Json :=
  let (row, kill) := Control.kill c.job c.control killObservation
  Json.mkObj [("name", toJson c.name), ("kind", toJson "kill"), ("job", jobJson c.job),
    ("observations", observationsJson c.job), ("control", controlJson c.control),
    ("expected", Json.mkObj [("row_state", rowStateJson row.state),
      ("won", toJson (match c.control.ends with
        | none => false
        | some state => decide ((_root_.PluginJob.terminalizeRow c.job state).2 =
            Background.CompletionDelivery.TerminalizationResult.won))),
      ("job_state", jobStateJson (JobState.of row.state c.control.note)),
      ("kill", killJson kill)])]

private def echoJson (e : ControlEcho) : Json :=
  Json.mkObj [("kind", toJson e.kind), ("payload", toJson (e.payload.map resumeJson))]

private def renderResume (c : ResumeCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("kind", toJson "resume"),
    ("caller", processScopeJson context.callerScope),
    ("owner", processScopeJson context.callerScope), ("same_plugin", toJson true),
    ("root", jobJson rowA), ("root_call_args", callArgsJson rowA.call),
    ("prior", jobJson priorJob),
    ("job", jobJson resumeCall), ("call_args", callArgsJson resumeCall.call),
    ("guest_pin", guestPinJson c.guest), ("facts", hostFactsJson c.facts),
    ("resume", resumeJson c.resume),
    ("expected", Json.mkObj [("valid", toJson (resumeValidFor c)),
      ("pin", if resumeValidFor c then pinJson (resumePinFor c) else Json.null),
      ("control", echoJson (echo c.resume)), ("root_call", rootCallJson (rootCallOf rowA))])]

private def renderResultRead (c : ResultReadCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("kind", toJson "result_read"),
    ("caller", processScopeJson c.caller), ("owner", processScopeJson context.callerScope),
    ("same_plugin", toJson c.samePlugin), ("terminal", toJson c.terminal),
    ("expected", Json.mkObj [("authorized", toJson (resultReadVerdict c))])]

private def invocationJson : PluginData.Invocation → Json
  | .toolCall => toJson "tool_call"
  | .jobStep => toJson "job_step"
  | .callback => toJson "callback"

private def renderResultOffer (c : String × PluginData.Invocation) : Json :=
  Json.mkObj [("name", toJson c.1), ("kind", toJson "result_offer"),
    ("invocation", invocationJson c.2),
    ("expected", Json.mkObj [("offered", toJson (resultOffered c.2))])]

private def renderStepFailure (c : StepFailureCase) : Json :=
  let (row, kill, refusal) := Step.onInvocationFailure (failingJob c) killObservation
  Json.mkObj [("name", toJson c.name), ("kind", toJson "step_failure"),
    ("job", jobJson (failingJob c)), ("consecutive_failures", toJson c.failures),
    ("observations", observationsJson (failingJob c)),
    ("expected", Json.mkObj [("row_state", rowStateJson row.state),
      ("consecutive_failures", toJson row.consecutiveFailures), ("kill", killJson kill),
      ("code", toJson (refusal.bind Refusal.code))])]

def controlCasesJson : String :=
  (toJson (killCases.map renderKill ++ resumeCases.map renderResume ++
    resultReadCases.map renderResultRead ++ resultOfferCases.map renderResultOffer ++
    stepFailureCases.map renderStepFailure)).compress

private def allKinds : List PackKind := [.documents, .assets, .plugins]

private def allRecords : List PluginJob.Record :=
  [.absent, .legacy, .written .operator, .written .selfConfig]

/-- Every record read for every pack kind. -/
def provenanceCases : List (PackKind × PluginJob.Record) :=
  allKinds.flatMap fun k => allRecords.map fun r => (k, r)

structure InstallCase where
  name : String
  prior : PluginJob.Record
  kind : PackKind
  explicit : Bool
  via : InstallProvenance

def installCases : List InstallCase :=
  [ ⟨"dependency_self_config_keeps_operator", .written .operator, .plugins, false, .selfConfig⟩
  , ⟨"dependency_self_config_keeps_legacy_operator", .legacy, .assets, false, .selfConfig⟩
  , ⟨"dependency_self_config_on_absent_writes_self_config", .absent, .plugins, false,
      .selfConfig⟩
  , ⟨"dependency_operator_raises_self_config", .written .selfConfig, .documents, false,
      .operator⟩
  , ⟨"explicit_self_config_lowers_operator", .written .operator, .plugins, true, .selfConfig⟩
  , ⟨"explicit_operator_raises_self_config", .written .selfConfig, .assets, true, .operator⟩ ]

private def fixturePack : String := "job_plugin_fixture"
private def scriptDigest : String := "sha256-flow"

/-- A pack-tier mount. Its wire `trusted` field is left unset: `trustedHint`
computes it. -/
private def packMount (via : InstallProvenance) (files : List String) : AssetMount :=
  { klass := .pack "workflows", pack := fixturePack, installDigest := "install-1"
  , path := "/cache/job_plugin_fixture/workflows", installed := true, trusted := false, via
  , files }

structure OriginCase where
  name : String
  digest : String
  mounts : List AssetMount
  trusted : List String

def originCases : List OriginCase :=
  [ ⟨"trusted_operator_pack_file_installed", scriptDigest,
      [packMount .operator [scriptDigest]], [fixturePack]⟩
  , ⟨"untrusted_pack_dynamic", scriptDigest, [packMount .operator [scriptDigest]],
      ["content_pack"]⟩
  , ⟨"self_config_pack_dynamic", scriptDigest, [packMount .selfConfig [scriptDigest]],
      [fixturePack]⟩
  , ⟨"project_mount_dynamic", scriptDigest,
      [{ projectMount "/work/.gents/workflows" with files := [scriptDigest] }], [fixturePack]⟩
  , ⟨"digest_not_in_any_mount_dynamic", scriptDigest, [packMount .operator ["sha256-other"]],
      [fixturePack]⟩ ]

private def originVerdict (c : OriginCase) : Origin × List Bool :=
  (originOf c.digest c.mounts c.trusted, c.mounts.map (trustedHint · c.trusted))

/-- The pack and its dependencies. -/
private def blockClosure : List String := [fixturePack, "content_pack"]

private def admissibleBlock : PackJobBlock :=
  { targetsFrom := ["content_pack"], trusted := ["content_pack"], dynamic := .check
  , projectRoots := [] }

structure PackBlockCase where
  name : String
  via : InstallProvenance
  block : PackJobBlock

def packBlockCases : List PackBlockCase :=
  [ ⟨"operator_block_within_closure_admitted", .operator, admissibleBlock⟩
  , ⟨"self_config_install_refused", .selfConfig, admissibleBlock⟩
  , ⟨"targets_outside_closure_refused", .operator,
      { admissibleBlock with targetsFrom := ["other_pack"] }⟩
  , ⟨"trusted_outside_closure_refused", .operator,
      { admissibleBlock with trusted := ["other_pack"] }⟩
  , ⟨"dynamic_run_refused", .operator, { admissibleBlock with dynamic := .run }⟩
  , ⟨"project_roots_refused", .operator, { admissibleBlock with projectRoots := ["/work"] }⟩ ]

structure PolicyCase where
  name : String
  agentTargets : List Target
  policy : PluginJob.Policy

def policyCases : List PolicyCase :=
  [ ⟨"off_ignores_non_subset_dynamic_targets", [targetA, targetB],
      { context.policy with mode := .off, dynamicTargets := scope [targetC] }⟩
  , ⟨"run_refuses_non_subset_dynamic_targets", [targetA, targetB],
      { context.policy with dynamicTargets := scope [targetC] }⟩
  , ⟨"check_admits_subset_dynamic_targets", [targetA, targetB],
      { context.policy with mode := .check }⟩
  , ⟨"default_empty_dynamic_targets_valid", [],
      { context.policy with dynamicTargets := scope [] }⟩ ]

structure WireCase where
  name : String
  pack : String
  packTargets : List Target
  policy : PluginJob.Policy
  jobTargets : List Target
  callerTargets : List Target

def wireCases : List WireCase :=
  [ ⟨"targets_from_trusts_pack_and_widens_targets", "content_pack", [targetC], context.policy,
      [targetA], [targetA, targetB]⟩
  , ⟨"targets_from_trusted_pack_listed_once", fixturePack, [targetB], context.policy, [targetA],
      [targetA]⟩ ]

private def wired (c : WireCase) : PluginJob.Policy × List Target × List Target :=
  wireTargetsFrom c.pack c.packTargets c.policy c.jobTargets c.callerTargets

/-- The wired policy's trusted packs, the job targets and the caller targets. -/
private def wireVerdict (c : WireCase) : List String × List Target × List Target :=
  let (p, job, caller) := wired c
  (p.trustedPacks, job, caller)

theorem authority_regressions :
    provenanceCases.map (fun (k, r) => provenanceOf k r) =
      [ .selfConfig, .selfConfig, .operator, .selfConfig
      , .selfConfig, .operator, .operator, .selfConfig
      , .selfConfig, .operator, .operator, .selfConfig ] ∧
    installCases.map (fun c => recordAfterInstall c.prior c.kind c.explicit c.via) =
      [ .written .operator, .written .operator, .written .selfConfig, .written .operator
      , .written .selfConfig, .written .operator ] ∧
    originCases.map originVerdict =
      [ (.installed, [true]), (.dynamic, [false]), (.dynamic, [false]), (.dynamic, [false])
      , (.dynamic, [true]) ] ∧
    packBlockCases.map (fun c => packJobBlockAdmissible c.via blockClosure c.block) =
      [true, false, false, false, false, false] ∧
    policyCases.map (fun c => c.policy.valid c.agentTargets) = [true, false, true, true] ∧
    wireCases.map wireVerdict =
      [ (["content_pack", "job_plugin_fixture"], [targetA, targetC], [targetB, targetA, targetC])
      , (["job_plugin_fixture"], [targetA, targetB], [targetA, targetB]) ] := by
  native_decide

private def provenanceJson : InstallProvenance → Json
  | .operator => toJson "operator"
  | .selfConfig => toJson "self_config"

private def recordJson : PluginJob.Record → Json
  | .absent => Json.mkObj [("kind", toJson "absent")]
  | .legacy => Json.mkObj [("kind", toJson "legacy")]
  | .written v => Json.mkObj [("kind", toJson "written"), ("via", provenanceJson v)]

private def kindName : PackKind → String
  | .documents => "documents"
  | .assets => "assets"
  | .plugins => "plugins"

private def recordName : PluginJob.Record → String
  | .absent => "absent"
  | .legacy => "legacy"
  | .written .operator => "written_operator"
  | .written .selfConfig => "written_self_config"

private def targetsJson (ts : List Target) : Json := toJson (ts.map targetJson)

private def mountJson (m : AssetMount) : Json :=
  Json.mkObj [("pack", toJson m.pack),
    ("class", toJson (match m.klass with | .pack k => k | .project => "project")),
    ("digest", toJson m.installDigest), ("path", toJson m.path), ("trusted", toJson m.trusted),
    ("via", provenanceJson m.via), ("files", toJson m.files)]

private def renderProvenance (c : PackKind × PluginJob.Record) : Json :=
  Json.mkObj [("name", toJson (kindName c.1 ++ "_" ++ recordName c.2)),
    ("kind", toJson "provenance"), ("pack_kind", toJson (kindName c.1)),
    ("record", recordJson c.2),
    ("expected", Json.mkObj [("provenance", provenanceJson (provenanceOf c.1 c.2))])]

private def renderInstall (c : InstallCase) : Json :=
  let r := recordAfterInstall c.prior c.kind c.explicit c.via
  Json.mkObj [("name", toJson c.name), ("kind", toJson "install"),
    ("prior", recordJson c.prior), ("pack_kind", toJson (kindName c.kind)),
    ("explicit", toJson c.explicit), ("via", provenanceJson c.via),
    ("expected", Json.mkObj [("record", recordJson r),
      ("provenance", provenanceJson (provenanceOf c.kind r))])]

private def renderOrigin (c : OriginCase) : Json :=
  let (origin, hints) := originVerdict c
  Json.mkObj [("name", toJson c.name), ("kind", toJson "origin"), ("digest", toJson c.digest),
    ("mounts", toJson (c.mounts.map mountJson)), ("trusted_packs", toJson c.trusted),
    ("expected", Json.mkObj [("origin", originJson origin), ("trusted_hints", toJson hints)])]

private def renderPackBlock (c : PackBlockCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("kind", toJson "pack_job_block"),
    ("via", provenanceJson c.via), ("closure", toJson blockClosure),
    ("block", Json.mkObj [("targets_from", toJson c.block.targetsFrom),
      ("trusted", toJson c.block.trusted), ("dynamic", modeJson c.block.dynamic),
      ("project_roots", toJson c.block.projectRoots)]),
    ("expected", Json.mkObj [("admissible",
      toJson (packJobBlockAdmissible c.via blockClosure c.block))])]

private def renderPolicy (c : PolicyCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("kind", toJson "policy_valid"),
    ("agent_targets", targetsJson c.agentTargets), ("policy", policyJson c.policy),
    ("expected", Json.mkObj [("valid", toJson (c.policy.valid c.agentTargets))])]

private def renderWire (c : WireCase) : Json :=
  let (p, job, caller) := wired c
  Json.mkObj [("name", toJson c.name), ("kind", toJson "wire_targets_from"),
    ("pack", toJson c.pack), ("pack_targets", targetsJson c.packTargets),
    ("policy", policyJson c.policy), ("job_targets", targetsJson c.jobTargets),
    ("caller_targets", targetsJson c.callerTargets),
    ("expected", Json.mkObj [("policy", policyJson p), ("job_targets", targetsJson job),
      ("caller_targets", targetsJson caller)])]

def authorityCasesJson : String :=
  (toJson (provenanceCases.map renderProvenance ++ installCases.map renderInstall ++
    originCases.map renderOrigin ++ packBlockCases.map renderPackBlock ++
    policyCases.map renderPolicy ++ wireCases.map renderWire)).compress

/-- A target's wire spelling is its AgentTarget id; the host resolves it to the
`(node, agent)` pair. -/
private def targetId (t : Target) : Json := toJson t.2

private def scopeTargets (sc : EndpointScope Target Unit) : Json :=
  toJson ((knownTargets.filter fun t => decide (sc.permits t)).map targetId)

/-- An optional field the protocol type omits when absent. -/
private def optField (k : String) : Option Json → List (String × Json)
  | some v => [(k, v)]
  | none => []

/-- A list field the protocol type omits when empty. -/
private def listField (k : String) (xs : List Json) : List (String × Json) :=
  if xs.isEmpty then [] else [(k, toJson xs)]

private def wireCeilingJson (c : Ceiling) : Json :=
  Json.mkObj [("targets", scopeTargets c.targets), ("max_live", toJson c.maxLive),
    ("max_spawns", toJson c.maxSpawns), ("max_messages_per_slot", toJson c.maxMessagesPerSlot),
    ("result_bytes", toJson c.resultBytes), ("max_wall_secs", toJson c.maxWallSecs)]

private def wirePinJson (p : Pin) : Json :=
  Json.mkObj ([("script_id", toJson p.scriptId), ("source_digest", toJson p.sourceDigest),
    ("origin", originJson p.origin), ("engine_digest", toJson p.engineDigest),
    ("plugin_ref", toJson p.pluginRef), ("agent_budget", toJson p.agentBudget),
    ("handle", toJson p.handle), ("cwd", toJson p.cwd), ("adopted_at", toJson p.adoptedAt),
    ("args_digest", toJson p.argsDigest), ("chain_root_doc_id", toJson p.chainRoot)] ++
    optField "resume_of" (p.resumeOf.map toJson) ++
    optField "replayed_from" (p.replayedFrom.map toJson) ++
    [("ceiling_at_pin", wireCeilingJson p.ceilingAtPin)])

private def wireResumeJson (r : Resume) : Json :=
  Json.mkObj ([("row_doc_id", toJson r.rowDoc)] ++
    optField "engine" (if r.engineCurrent then some (toJson "current") else none) ++
    optField "answer" (r.answer.map toJson) ++ optField "budget" (r.budget.map toJson))

/-- The model's `args` text is a JSON object, echoed verbatim. -/
private def argsJson (s : String) : Json := (Json.parse s).toOption.getD (toJson s)

private def wireRootCallJson (c : RootCall) : Json :=
  Json.mkObj ([("argv", toJson c.argv)] ++ optField "args" (c.args.map argsJson) ++
    optField "path" (c.path.map toJson) ++ optField "agent_budget" (c.agentBudget.map toJson))

private def wireEchoJson (e : ControlEcho) : Json :=
  Json.mkObj ([("kind", toJson e.kind)] ++ optField "payload" (e.payload.map wireResumeJson))

/-- A mount renders only its wire fields; `files` is model-only. -/
private def wireMountJson (trusted : List String) (m : AssetMount) : Json :=
  Json.mkObj [("pack", toJson m.pack),
    ("class", toJson (match m.klass with | .pack k => k | .project => "project")),
    ("digest", toJson m.installDigest), ("path", toJson m.path),
    ("trusted", toJson (trustedHint m trusted))]

private def usageJson : Json :=
  Json.mkObj [("input", toJson (120 : Nat)), ("output", toJson (80 : Nat)),
    ("total", toJson (200 : Nat))]

private def childViewJson (ch : Child) : Json :=
  Json.mkObj [("attempt", toJson ch.key.attempt), ("session_id", toJson ch.key.sessionId),
    ("request_id", toJson ("request-" ++ ch.key.slot)), ("title", toJson ch.key.slot),
    ("state", childStateJson ch.state), ("output_bytes", toJson (42 : Nat)),
    ("truncated", toJson false), ("usage", usageJson), ("duration_ms", toJson (1500 : Nat))]

/-- Guest progress over row `j` with an agent budget of `total`. -/
private def progressJson (j : Job) (total : Nat) (state : JobState)
    (reason : Option PauseReason) (lastEvent : String) : Json :=
  Json.mkObj ([("title", toJson j.handle), ("state", jobStateJson state)] ++
    optField "pause_reason" (reason.map pauseReasonJson) ++
    [("phases", toJson [Json.mkObj [("title", toJson "review"), ("state", toJson "active")]]),
     ("active", toJson ((j.children.filter (·.state == .live)).map fun ch =>
        Json.mkObj [("slot", toJson ch.key.slot), ("title", toJson ch.key.slot),
          ("session_id", toJson ch.key.sessionId)])),
     ("spawned", toJson j.spent),
     ("budget", Json.mkObj [("total", toJson total), ("spent", toJson j.spent),
        ("remaining", toJson (Step.remainingSpawns total j))]),
     ("last_event", toJson lastEvent)] ++
    optField "question" (if state == .awaitingUser then some (toJson "Proceed?") else none) ++
    [("observed_terminal_children", toJson j.observedTerminal), ("revision", toJson (0 : Nat))])

/-- The fixture script's resolved tier and origin, as an adopting step's
`last_event` reports them. -/
private def adoptionLine : String :=
  "tier: pack " ++ fixturePack ++ "; origin: " ++
    match originOf scriptDigest [packMount .operator [scriptDigest]] [fixturePack] with
    | .installed => "installed"
    | .dynamic => "dynamic"

structure OutputSample where
  name : String
  /-- The row the step runs on; the progress counts come from it. -/
  job : Job
  out : StepOutput
  /-- The `JobProgress.state` the guest reports. -/
  state : JobState

def outputSamples : List OutputSample :=
  [ ⟨"result_every_rider", pinned [] 0 0,
      .result { writes := 1, scratch := ["runs/call-1/report.md"], progress := true }
        [("row-b", .stop), ("row-a", .pause)] none none, .completed⟩
  , ⟨"fail_every_rider", pinned [] 0 0,
      .fail { writes := 1, scratch := [], progress := true }, .failed⟩
  , ⟨"suspend_pause_every_rider", pinned [] 0 0,
      .suspend .pause (some .verification)
        { writes := 1, scratch := ["runs/call-1/report.md"], progress := true } none none,
      .paused⟩
  , ⟨"suspend_await_user", pinned [] 0 0, .suspend .awaitUser none noExtras none none,
      .awaitingUser⟩
  , ⟨"step0_effects_every_rider_adopting", stepZero,
      .effects { pin := some guestPin
               , resume := some { resumeBlock with engineCurrent := true, budget := some 5 }
               , spawns := [spawn "1:a" targetA, { spawn "2:b" targetB with cwd := some "/work/sub" }]
               , messages := ["1:a"], interrupts := ["2:b"]
               , extras := { writes := 1, scratch := ["runs/call-1/plan.md"], progress := true } },
      .running⟩
  , ⟨"step0_result_with_pin", stepZero, .result noExtras [] (some guestPin) none, .completed⟩
  , ⟨"step0_suspend_with_pin", stepZero,
      .suspend .pause (some .user) noExtras (some guestPin) none, .paused⟩
  , ⟨"step0_result_with_pin_and_resume", stepZero,
      .result noExtras [] (some guestPin) (some resumeBlock), .completed⟩
  , ⟨"step0_suspend_with_pin_and_resume", stepZero,
      .suspend .awaitUser none noExtras (some guestPin) (some answerResume), .awaitingUser⟩ ]

private def sampleProgress (s : OutputSample) : Json :=
  let total := match s.job.pin, s.out.pin with
    | some p, _ => p.agentBudget
    | none, some g => g.agentBudget
    | none, none => 0
  let reason := match s.out with
    | .suspend _ r _ _ _ => r
    | _ => none
  progressJson s.job total s.state reason (if s.out.pin.isSome then adoptionLine else "step")

private def writesJson (n : Nat) : List Json :=
  (List.range n).map fun i => Json.mkObj [("op", toJson "insert"),
    ("collection", toJson "FixtureNote"), ("fields", Json.mkObj [("title", toJson s!"note-{i}")])]

private def ridersJson (s : OutputSample) (x : Extras) : List (String × Json) :=
  listField "writes" (writesJson x.writes) ++
    listField "scratch" (x.scratch.map fun n =>
      Json.mkObj [("name", toJson n), ("content", toJson "draft")]) ++
    optField "progress" (if x.progress then some (sampleProgress s) else none)

private def pinResumeJson (pin : Option GuestPin) (resume : Option Resume) :
    List (String × Json) :=
  optField "pin" (pin.map guestPinJson) ++ optField "resume" (resume.map wireResumeJson)

private def wireSpawnJson (s : Spawn) : Json :=
  Json.mkObj ([("slot", toJson s.slot), ("attempt", toJson s.attempt),
    ("target", targetId s.target), ("prompt", toJson ("review " ++ s.slot)),
    ("title", toJson s.slot)] ++ optField "cwd" (s.cwd.map toJson))

private def wireOutputJson (s : OutputSample) : Json :=
  match s.out with
  | .result x control pin resume => Json.mkObj ([("result", toJson "done")] ++ ridersJson s x ++
      listField "control" (control.map fun (row, k) => Json.mkObj [("row_doc_id", toJson row),
        ("kind", toJson (match k with | .stop => "stop" | .pause => "pause"))]) ++
      pinResumeJson pin resume)
  | .fail x => Json.mkObj ([("fail", Json.mkObj [("code", toJson "engine_error"),
      ("message", toJson "the script raised")])] ++ ridersJson s x)
  | .suspend k reason x pin resume => Json.mkObj ([("suspend", Json.mkObj
      ([("kind", toJson (match k with | .pause => "pause" | .awaitUser => "await_user"))] ++
        optField "reason" (reason.map pauseReasonJson) ++ [("message", toJson "waiting")] ++
        optField "question" (if k == .awaitUser then some (toJson "Proceed?") else none)))] ++
      ridersJson s x ++ pinResumeJson pin resume)
  | .effects e => Json.mkObj [("effects", Json.mkObj (pinResumeJson e.pin e.resume ++
      listField "spawn" (e.spawns.map wireSpawnJson) ++
      listField "message" (e.messages.map fun slot =>
        Json.mkObj [("slot", toJson slot), ("text", toJson "more detail")]) ++
      listField "interrupt" (e.interrupts.map fun slot => Json.mkObj [("slot", toJson slot)]) ++
      ridersJson s e.extras))]

/-- One `jobs[]` row: its persisted state and terminal note, its pin's
adoption time, and the row its pin resumes. -/
structure RowSample where
  (rowDoc toolCall handle : String)
  state : RowState
  note : TerminalNote
  adoptedAt : Nat
  resumeOf : Option DocId

structure InputSample where
  name : String
  job : Job
  /-- The step number `verifiedScript` reads. -/
  step : Nat
  mounts : List AssetMount
  jobs : List RowSample
  scratch : List (String × String)

private def verifiedSource : String := "workflow flow { verified }"

def inputSamples : List InputSample :=
  [ ⟨"model_script_beside_verified_script",
      { pinned [child "1:a" .live] 1 0 with
        call := { argv := ["run"], args := some "{\"topic\":\"wire\"}", path := none
                , script := some "workflow flow { model }", agentBudget := some 3 } },
      1, [packMount .operator [scriptDigest], projectMount "/work/.gents/workflows"], [],
      [("runs/call-1/notes.md", "draft")]⟩
  , ⟨"resumed_step0_echoes_control_and_root_call",
      resumeRow rowA priorJob
        { resumeCall with call := { resumeCall.call with args := some "{\"own\":true}"
                                                       , path := some "flows/flow.rhai" } }
        answerResume,
      0, [], [], []⟩
  , ⟨"paused_row_jobs_and_child_states",
      pinned [child "1:a" .live, child "2:b" .completed, child "3:c" .failed,
        child "4:d" .interrupted, child "5:e" .other] 5 4,
      2, [], [⟨"row-p", "call-p", "flow-p", .completed, .paused (some .user), 1000, none⟩,
        ⟨"row-w", "call-w", "flow-w", .completed, .awaitingUser, 2000, none⟩,
        ⟨"row-c", "call-c", "flow-p", .completed, .none, 3000, some "row-p"⟩], []⟩ ]

private def verified (s : InputSample) : Option VerifiedScript :=
  s.job.pin.bind fun p => verifiedScript p s.step verifiedSource

private def wireInputJson (s : InputSample) : Json :=
  let j := s.job
  let echo := stepEcho j
  let c := context
  Json.mkObj ([("protocol", toJson (1 : Nat)),
    ("job", Json.mkObj [("row_doc_id", toJson j.rowDoc), ("chain_root_doc_id", toJson j.root),
      ("chain", toJson (if j.root == j.rowDoc then [j.rowDoc] else [j.root, j.rowDoc])),
      ("handle", toJson j.handle)]),
    ("caller", Json.mkObj [("session_id", toJson j.callerSession),
      ("request_id", toJson c.callerScope.requestId), ("request_doc_id", toJson "request-doc-1"),
      ("tool_call_id", toJson "call-1"), ("node_did", toJson c.callerScope.nodeDid),
      ("agent_id", toJson "engineer"), ("request_hop", toJson j.callerHop),
      ("correlation", Json.null), ("cwd", toJson j.callerCwd),
      ("started_by_job", toJson c.startedByJob)]),
    ("argv", toJson j.call.argv)] ++
    optField "args" (j.call.args.map argsJson) ++ optField "path" (j.call.path.map toJson) ++
    optField "script" (j.call.script.map toJson) ++
    optField "agent_budget" (j.call.agentBudget.map toJson) ++
    optField "root_call" (echo.rootCall.map wireRootCallJson) ++
    [("pin", toJson (j.pin.map wirePinJson)),
     ("origin", toJson (j.pin.map fun p => originJson p.origin))] ++
    optField "verified_script" ((verified s).map fun v =>
      Json.mkObj [("source", toJson v.source), ("digest", toJson v.digest)]) ++
    [("mounts", toJson (s.mounts.map (wireMountJson c.policy.trustedPacks))),
     ("ceiling", Json.mkObj [("targets", scopeTargets c.live.targets),
        ("max_live", toJson c.live.maxLive),
        ("remaining_spawns", toJson (Step.remainingSpawns c.live.maxSpawns j)),
        ("max_messages_per_slot", toJson c.live.maxMessagesPerSlot),
        ("result_bytes", toJson c.live.resultBytes), ("dynamic", modeJson c.policy.mode),
        ("dynamic_targets", scopeTargets c.policy.dynamicTargets)]),
     ("children", Json.mkObj (j.children.map fun ch => (ch.key.slot, childViewJson ch))),
     ("control", toJson (echo.control.map wireEchoJson)),
     ("scratch", Json.mkObj (s.scratch.map fun (n, v) => (n, toJson v))),
     ("jobs", toJson (s.jobs.map fun r => Json.mkObj ([("row_doc_id", toJson r.rowDoc),
        ("tool_call_id", toJson r.toolCall), ("handle", toJson r.handle),
        ("state", jobStateJson (JobState.of r.state r.note)),
        ("adopted_at", toJson r.adoptedAt)] ++ optField "resume_of" (r.resumeOf.map toJson))))])

/-- A row driven from a child session, whose id contains `:`. -/
private def viewJob : Job :=
  { pinned [child "1:a" .completed, child "2:b" .failed] 2 2 with
    rowDoc := "row-v", callerSession := childSession, state := .failed }

private def viewAddress : String := childSession ++ ":" ++ "call-7"

private def viewJson : Json :=
  let j := viewJob
  let p := hostPin guestPin facts
  let state := JobState.of j.state .none
  Json.mkObj [("address", toJson viewAddress), ("row_doc_id", toJson j.rowDoc),
    ("session_id", toJson j.callerSession), ("tool_call_id", toJson "call-7"),
    ("scope", Json.mkObj [("session_id", toJson j.callerSession),
      ("request_id", toJson context.callerScope.requestId),
      ("node_did", toJson context.callerScope.nodeDid),
      ("requester_did", toJson "did:key:user")]),
    ("handle", toJson j.handle), ("state", jobStateJson state),
    ("outcome", Json.mkObj [("code", toJson ((Refusal.code .stalled).getD "")),
      ("message", toJson "no live child and no new effect")]),
    ("pin", Json.mkObj ([("script_id", toJson p.scriptId), ("origin", originJson p.origin),
      ("engine_digest", toJson p.engineDigest), ("plugin_ref", toJson p.pluginRef),
      ("agent_budget", toJson p.agentBudget), ("cwd", toJson p.cwd),
      ("chain_root_doc_id", toJson p.chainRoot)] ++ optField "resume_of" (p.resumeOf.map toJson))),
    ("progress", progressJson j p.agentBudget state none "stalled"),
    ("children", toJson (j.children.map fun ch => Json.mkObj [("slot", toJson ch.key.slot),
      ("attempt", toJson ch.key.attempt), ("session_id", toJson ch.key.sessionId),
      ("request_id", toJson ("request-" ++ ch.key.slot)), ("state", childStateJson ch.state),
      ("title", toJson ch.key.slot), ("usage", usageJson), ("duration_ms", toJson (1500 : Nat))])),
    ("created_at", toJson (1000 : Nat)), ("adopted_at", toJson p.adoptedAt),
    ("completed_at", toJson (2000 : Nat))]

/-- The nine host terminal codes; `.terminal` has none. -/
def hostCodes : List String :=
  ([.terminal, .notAvailable, .engineChanged, .policyDenied, .stalled, .depthExceeded, .timedOut,
    .stepFailed, .resumeUnknown, .resumeNotHead] : List Refusal).filterMap Refusal.code

/-- The origin, the verified script's digest, the echoed control kind, whether
`root_call` is echoed, the `jobs[]` states and the mount trust hints. -/
structure InputVerdict where
  origin : Option Origin
  digest : Option String
  echoKind : Option String
  rootCall : Bool
  jobs : List JobState
  hints : List Bool
  deriving DecidableEq

private def inputVerdict (s : InputSample) : InputVerdict :=
  ⟨s.job.pin.map (·.origin), (verified s).map (·.digest), (stepEcho s.job).control.map (·.kind),
    (stepEcho s.job).rootCall.isSome, s.jobs.map (fun r => JobState.of r.state r.note),
    s.mounts.map (trustedHint · context.policy.trustedPacks)⟩

theorem wire_regressions :
    hostCodes =
      [ "plugin_job_not_available", "plugin_job_engine_changed", "plugin_job_policy_denied"
      , "plugin_job_stalled", "plugin_job_depth_exceeded", "plugin_job_timed_out"
      , "plugin_job_step_failed", "plugin_job_resume_unknown", "plugin_job_resume_not_head" ] ∧
    inputSamples.map inputVerdict =
      [ ⟨some .installed, some "source-1", none, false, [], [true, false]⟩
      , ⟨none, none, some "answer", true, [], []⟩
      , ⟨some .installed, some "source-1", none, false, [.paused, .awaitingUser, .completed],
          []⟩ ] ∧
    outputSamples.map (fun s => (s.out.pin.isSome, s.out.resume.isSome)) =
      [ (false, false), (false, false), (false, false), (false, false), (true, true)
      , (true, false), (true, false), (true, true), (true, true) ] ∧
    adoptionLine = "tier: pack job_plugin_fixture; origin: installed" ∧
    splitAddress viewAddress = some (childSession, "call-7") := by
  native_decide

private def wireCase (name kind : String) (value : Json) : Json :=
  Json.mkObj [("name", toJson name), ("kind", toJson kind), ("value", value)]

def wireCasesJson : String :=
  (toJson (inputSamples.map (fun s => wireCase s.name "job_step_input" (wireInputJson s)) ++
    outputSamples.map (fun s => wireCase s.name "job_step_output" (wireOutputJson s)) ++
    [wireCase "job_view_session_with_colons" "job_view" viewJson] ++
    hostCodes.map fun code => wireCase code "job_outcome" (Json.mkObj
      [("code", toJson code), ("message", toJson "host terminal")]))).compress

/-- A request row: created at `t`, in `session`, public, with its calling
request and tool-call edges. -/
private def request (t : Nat) (doc session : String) (cause call : Option String) :
    DurableLineage.RequestRow :=
  { createdAt := t, docId := doc, session := session, isPublic := true
  , causeRequestDoc := cause, causeToolCallDoc := call }

/-- A request the job's caller `req-caller` caused through the job's tool call. -/
private def jobRequest (t : Nat) (doc session : String) : DurableLineage.RequestRow :=
  request t doc session (some "req-caller") (some "call-job")

/-- A job request in a session that recorded `opening` as its opening request
when it was created. -/
private def recordedRequest (t : Nat) (doc session opening : String) : DurableLineage.RequestRow :=
  { jobRequest t doc session with sessionOpening := some opening }

/-- Request rows, and the `(cause, session)` pairs whose opening request is
asked for. -/
structure LineageCase where
  name : String
  rows : List DurableLineage.RequestRow
  queries : List (String × String)

def lineageCases : List LineageCase :=
  [ ⟨"job_children_open_one_session_each",
      [ request 1 "req-caller" "parent" none none, jobRequest 10 "req-a" "child-a"
      , jobRequest 11 "req-b" "child-b", jobRequest 12 "req-c" "child-c" ],
      [("req-caller", "child-a"), ("req-caller", "child-b"), ("req-caller", "child-c")]⟩
  , ⟨"slot_message_lands_after_the_opening_request",
      [jobRequest 20 "req-a-message" "child-a", jobRequest 10 "req-a" "child-a"],
      [("req-caller", "child-a")]⟩
  , ⟨"reattempt_opens_new_session",
      [jobRequest 10 "req-a" "child-a-1", jobRequest 30 "req-a-retry" "child-a-2"],
      [("req-caller", "child-a-1"), ("req-caller", "child-a-2")]⟩
  , ⟨"created_at_tie_broken_by_doc_id",
      [jobRequest 10 "req-b" "child-a", jobRequest 10 "req-a" "child-a"],
      [("req-caller", "child-a")]⟩
  , ⟨"null_tool_call_edge_never_opening",
      [ request 5 "req-a-uncaused" "child-a" (some "req-caller") none
      , jobRequest 10 "req-a" "child-a"
      , request 5 "req-b-uncaused" "child-b" (some "req-caller") none ],
      [("req-caller", "child-a"), ("req-caller", "child-b")]⟩
  , ⟨"recorded_opening_beats_a_same_second_message",
      [recordedRequest 10 "req-a-message" "child-a" "req-b", recordedRequest 10 "req-b" "child-a" "req-b"],
      [("req-caller", "child-a")]⟩
  , ⟨"recorded_opening_not_yet_visible",
      [recordedRequest 20 "req-a-message" "child-a" "req-a"],
      [("req-caller", "child-a")]⟩ ]

private def openingDocs (c : LineageCase) : List (Option String) :=
  c.queries.map fun (cause, session) =>
    (DurableLineage.openingRequest c.rows cause session).map (·.docId)

theorem lineage_regressions :
    lineageCases.map openingDocs =
      [ [some "req-a", some "req-b", some "req-c"], [some "req-a"]
      , [some "req-a", some "req-a-retry"], [some "req-a"], [some "req-a", none], [some "req-b"], [none] ] := by
  native_decide

private def requestRowJson (r : DurableLineage.RequestRow) : Json :=
  Json.mkObj [("created_at", toJson r.createdAt), ("_docID", toJson r.docId),
    ("session_id", toJson r.session), ("is_public", toJson r.isPublic),
    ("caused_by_parent_request_doc_id", toJson r.causeRequestDoc),
    ("caused_by_parent_tool_call_doc_id", toJson r.causeToolCallDoc),
    ("opening_request_doc_id", toJson r.sessionOpening)]

private def renderLineage (c : LineageCase) : Json :=
  Json.mkObj [("name", toJson c.name), ("rows", toJson (c.rows.map requestRowJson)),
    ("queries", toJson ((c.queries.zip (openingDocs c)).map fun ((cause, session), opening) =>
      Json.mkObj [("cause_request_doc_id", toJson cause), ("session_id", toJson session),
        ("expected_opening_doc_id", toJson opening)]))]

def lineageCasesJson : String := (toJson (lineageCases.map renderLineage)).compress

/-- One plugin-job row before a sweep tick or a stale observer invocation. -/
structure RecoveryCase where
  name : String
  row : Recovery.PluginJobRow

private def jobRow (terminalChildren observed live stepNew : Nat)
    (deadlineExpired := false) (available := true) (stepped := true) : Recovery.PluginJobRow :=
  { running := true, sessionStopped := false, terminalChildren, observed, live, stepNew,
    deadlineExpired, available, stepped }

def recoveryCases : List RecoveryCase :=
  [ ⟨"plugin_job_unobserved_terminal_step_with_new_effects_keeps_running", jobRow 2 1 0 1⟩
  , ⟨"plugin_job_unobserved_terminal_step_without_new_effects_stalls", jobRow 2 1 0 0⟩
  , ⟨"plugin_job_unobserved_terminal_with_live_child_keeps_running", jobRow 2 1 1 0⟩
  , ⟨"plugin_job_deadline_expired_fails_closed", jobRow 1 1 1 0 (deadlineExpired := true)⟩
  , ⟨"plugin_job_unavailable_fails_closed", jobRow 1 1 1 0 (available := false)⟩
  , ⟨"plugin_job_stalled_at_rest_fails_closed", jobRow 2 2 0 0⟩
  , ⟨"plugin_job_live_child_not_stale", jobRow 1 1 1 0⟩
  , ⟨"plugin_job_stopped_session_kills_row",
      { jobRow 1 1 1 0 with sessionStopped := true }⟩
  , ⟨"plugin_job_terminal_stale_kick_does_not_invoke_guest",
      { jobRow 0 0 0 0 with running := false }⟩
  , ⟨"plugin_job_resumed_row_first_step_with_new_effects_keeps_running",
      jobRow 1 1 0 1 (stepped := false)⟩
  , ⟨"plugin_job_resumed_row_first_step_without_new_effects_stalls",
      jobRow 1 1 0 0 (stepped := false)⟩ ]

/-- One tick of the registered sweep. -/
def recoveryTick (row : Recovery.PluginJobRow) : Recovery.PluginJobRow :=
  Recovery.pluginJobRecoverySweep.recover row

def recoveryStale (row : Recovery.PluginJobRow) : Bool :=
  @decide (Recovery.pluginJobRecoverySweep.stale row)
    (inferInstanceAs (Decidable
      (Recovery.pluginJobStale row ∨ Recovery.pluginJobStoppedSessionStale row)))

def recoveryMeasure (row : Recovery.PluginJobRow) : Nat :=
  Recovery.pluginJobRecoverySweep.measure row

private def recoveryVerdict (c : RecoveryCase) : Bool × Nat × Nat × Bool × Nat :=
  let after := recoveryTick c.row
  (recoveryStale c.row, recoveryMeasure c.row, recoveryMeasure after, after.running, after.observed)

theorem recovery_regressions :
    recoveryCases.map recoveryVerdict =
      [ (true, 1, 0, true, 2), (true, 1, 0, false, 2), (true, 1, 0, true, 2)
      , (true, 1, 0, false, 1), (true, 1, 0, false, 1), (true, 1, 0, false, 2)
      , (false, 0, 0, true, 1), (true, 1, 0, false, 1)
      , (false, 0, 0, false, 0), (true, 1, 0, true, 1), (true, 1, 0, false, 1) ] := by
  native_decide

private def jobRowJson (r : Recovery.PluginJobRow) : Json :=
  Json.mkObj [("running", toJson r.running), ("session_stopped", toJson r.sessionStopped),
    ("terminal_children", toJson r.terminalChildren),
    ("observed", toJson r.observed), ("live", toJson r.live), ("step_new", toJson r.stepNew),
    ("deadline_expired", toJson r.deadlineExpired), ("available", toJson r.available),
    ("stepped", toJson r.stepped)]

private def renderRecovery (c : RecoveryCase) : Json :=
  let after := recoveryTick c.row
  Json.mkObj [("name", toJson c.name),
    ("sweep_id", toJson Recovery.pluginJobRecoverySweep.sweepId),
    ("rust_function", toJson Recovery.pluginJobRecoverySweep.rustFunction),
    ("row", jobRowJson c.row), ("stale", toJson (recoveryStale c.row)),
    ("measure_before", toJson (recoveryMeasure c.row)),
    ("measure_after", toJson (recoveryMeasure after)),
    ("step_commit_allowed", toJson (Recovery.pluginJobStepCommitAllowed c.row.sessionStopped)),
    ("step_invocation_allowed", toJson (Recovery.pluginJobStepInvocationAllowed
      c.row.running c.row.sessionStopped)),
    ("expected_row", jobRowJson after)]

def recoveryCasesJson : String := (toJson (recoveryCases.map renderRecovery)).compress

end Conformance.PluginJob
