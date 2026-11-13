import Proofs.Conformance.Triggers.Materialization
import Proofs.Runtime.Properties.Safety
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.TriggerContracts

open Conformance.Contracts

structure TriggerScenario where
  name : String
  snap : TriggerSnapshot
  before : SystemState
  intent : FireIntent

def concurrencyName : ConcurrencyMode → String
  | .parallel => "parallel"
  | .serial => "serial"
  | .queuedSerial => "queued_serial"
  | .latestOnly => "latest_only"

def jsonOptionString : Option String → String := jsonOptionalString

def jsonOptionNat : Option Nat → String
  | none => "null"
  | some value => toString value

def keyJson (key : TriggerKey) : String := jsonString key

def schedule (triggerId : String) : ActiveSchedule :=
  { triggerId := triggerId, taskId := "task", enabled := true }

def eventTrigger (triggerId : String) : ActiveEventTrigger :=
  { triggerId := triggerId
  , taskId := "task"
  , sourceCollection := "WebhookEvent"
  , eventKind := "created"
  , enabled := true
  , concurrency := .serial
  }

def snapshot (scheduleIds eventIds : List String) : TriggerSnapshot :=
  { generation := 1
  , activeSchedules := scheduleIds.map schedule
  , activeEventTriggers := eventIds.map eventTrigger
  }

def intent
    (triggerId : Option String)
    (triggerKind : TriggerKind)
    (concurrency : ConcurrencyMode) : FireIntent :=
  { triggerId := triggerId
  , triggerKind := triggerKind
  , taskId := "task"
  , concurrency := concurrency
  }

def request
    (id triggerId : String)
    (triggerKind : TriggerKind)
    (concurrency : ConcurrencyMode)
    (isTerminal : Bool) : AgentRequest :=
  { id := id
  , causedBy := some triggerId
  , concurrency := concurrency
  , isTerminal := isTerminal
  , executionOrigin :=
      match triggerKind with
      | .manual => .interactive
      | .schedule | .event => .scheduled
  }

def after (scenario : TriggerScenario) : SystemState :=
  dispatchStep scenario.before scenario.snap scenario.intent

def targetKey? (scenario : TriggerScenario) : Option TriggerKey :=
  scenario.intent.triggerId

def newRequest? (scenario : TriggerScenario) : Option AgentRequest :=
  (after scenario).requests[scenario.before.requests.length]?

def causedById? (request : AgentRequest) : Option String := request.causedBy

def causedByKind? (scenario : TriggerScenario) : Option String :=
  (dispatch scenario.snap scenario.intent).bind fun seed =>
    seed.causedByTriggerId.map fun _ => seed.causedByTriggerKind.toDefraDB

def expectedResult (scenario : TriggerScenario) : String :=
  if scenario.before.requests.length < (after scenario).requests.length then
    "fired"
  else
    "skipped"

def expectedSkipReason (scenario : TriggerScenario) : Option String :=
  if expectedResult scenario = "fired" then
    none
  else
    match dispatch scenario.snap scenario.intent with
    | none => some "trigger disabled"
    | some _ =>
      match scenario.intent.concurrency with
      | .serial => some "serial: prior fire still in-flight"
      | .parallel | .queuedSerial | .latestOnly => none

def expectedSupersedeCallKeys (scenario : TriggerScenario) : List TriggerKey :=
  match dispatch scenario.snap scenario.intent, scenario.intent.concurrency, scenario.intent.triggerId with
  | some _, .latestOnly, some triggerId => [triggerId]
  | _, _, _ => []

def priorNonterminalKeys (scenario : TriggerScenario) : List TriggerKey :=
  scenario.before.requests.filterMap fun request =>
    if request.isTerminal then
      none
    else
      request.causedBy

def supersededPriorIds (scenario : TriggerScenario) : List String :=
  match targetKey? scenario with
  | none => []
  | some key =>
    scenario.before.requests.filterMap fun request =>
      if (request.causedBy == some key) && !request.isTerminal then
        if (after scenario).requests.any
            (fun post => (post.id == request.id) && post.isTerminal) then
          some request.id
        else
          none
      else
        none

def targetNonterminalCountAfter? (scenario : TriggerScenario) : Option Nat :=
  targetKey? scenario |>.map fun key =>
    (after scenario).nonTerminalCountFor key

def triggerDispatchScenarios : List TriggerScenario :=
  [ { name := "manual_unconditional"
    , snap := snapshot [] []
    , before := SystemState.empty
    , intent := intent none .manual .parallel
    }
  , { name := "schedule_uses_configured_task"
    , snap := snapshot ["sched-a"] []
    , before := SystemState.empty
    , intent := { intent (some "sched-a") .schedule .parallel with taskId := "stale-task" }
    }
  , { name := "event_uses_configured_task"
    , snap := snapshot [] ["event-a"]
    , before := SystemState.empty
    , intent := { intent (some "event-a") .event .parallel with taskId := "stale-task" }
    }
  , { name := "schedule_disabled_is_unreachable"
    , snap := snapshot [] []
    , before := SystemState.empty
    , intent := intent (some "sched-a") .schedule .serial
    }
  , { name := "event_disabled_is_unreachable"
    , snap := snapshot [] []
    , before := SystemState.empty
    , intent := intent (some "event-a") .event .serial
    }
  , { name := "schedule_serial_clear_fires"
    , snap := snapshot ["sched-a"] []
    , before := SystemState.empty
    , intent := intent (some "sched-a") .schedule .serial
    }
  , { name := "event_serial_clear_fires"
    , snap := snapshot [] ["event-a"]
    , before := SystemState.empty
    , intent := intent (some "event-a") .event .serial
    }
  , { name := "schedule_serial_same_id_skips"
    , snap := snapshot ["sched-a"] []
    , before := { requests := [request "prior-schedule" "sched-a" .schedule .serial false] }
    , intent := intent (some "sched-a") .schedule .serial
    }
  , { name := "schedule_serial_same_id_other_kind_skips"
    , snap := snapshot ["shared"] []
    , before := { requests := [request "prior-event" "shared" .event .serial false] }
    , intent := intent (some "shared") .schedule .serial
    }
  , { name := "event_serial_same_id_skips"
    , snap := snapshot [] ["event-a"]
    , before := { requests := [request "prior-event" "event-a" .event .serial false] }
    , intent := intent (some "event-a") .event .serial
    }
  , { name := "schedule_parallel_ignores_prior_inflight"
    , snap := snapshot ["sched-a"] []
    , before := { requests := [request "prior-schedule" "sched-a" .schedule .serial false] }
    , intent := intent (some "sched-a") .schedule .parallel
    }
  , { name := "schedule_latest_only_clear_fires_with_supersede_call"
    , snap := snapshot ["sched-a"] []
    , before := SystemState.empty
    , intent := intent (some "sched-a") .schedule .latestOnly
    }
  , { name := "schedule_latest_only_supersedes_prior"
    , snap := snapshot ["sched-a"] []
    , before := { requests := [request "prior-schedule" "sched-a" .schedule .latestOnly false] }
    , intent := intent (some "sched-a") .schedule .latestOnly
    }
  , { name := "event_latest_only_supersedes_prior_schedule_same_id"
    , snap := snapshot [] ["shared"]
    , before := { requests := [request "prior-schedule" "shared" .schedule .serial false] }
    , intent := intent (some "shared") .event .latestOnly
    }
  , { name := "event_latest_only_supersedes_prior"
    , snap := snapshot [] ["event-a"]
    , before := { requests := [request "prior-event" "event-a" .event .latestOnly false] }
    , intent := intent (some "event-a") .event .latestOnly
    }
  , { name := "manual_latest_only_without_key_fires_without_supersede"
    , snap := snapshot [] []
    , before := { requests := [request "prior-schedule" "sched-a" .schedule .latestOnly false] }
    , intent := intent none .manual .latestOnly
    }
  ]

deriving instance Lean.ToJson for TriggerKind, ConcurrencyMode, ExecutionOrigin, AgentRequest

structure DispatchInput where
  name : String
  triggerId : Option String
  triggerKind : TriggerKind
  intentTaskId : String
  concurrency : ConcurrencyMode
  activeScheduleIds : List String
  activeEventTriggerIds : List String
  priorRequests : List AgentRequest
  priorNonterminalKeys : List String
  requestCountBefore : Nat
  deriving Lean.ToJson

def DispatchInput.ofScenario (scenario : TriggerScenario) : DispatchInput :=
  { name := scenario.name, triggerId := scenario.intent.triggerId
    triggerKind := scenario.intent.triggerKind, intentTaskId := scenario.intent.taskId
    concurrency := scenario.intent.concurrency
    activeScheduleIds := scenario.snap.activeSchedules.map ActiveSchedule.triggerId
    activeEventTriggerIds := scenario.snap.activeEventTriggers.map ActiveEventTrigger.triggerId
    priorRequests := scenario.before.requests
    priorNonterminalKeys := Conformance.TriggerContracts.priorNonterminalKeys scenario
    requestCountBefore := scenario.before.requests.length }

def DispatchInput.scenario (input : DispatchInput) : TriggerScenario :=
  { name := input.name
    snap := snapshot input.activeScheduleIds input.activeEventTriggerIds
    before := { requests := input.priorRequests }
    intent := { triggerId := input.triggerId, triggerKind := input.triggerKind
                taskId := input.intentTaskId, concurrency := input.concurrency } }

structure DispatchOutput where
  result : String
  skipReason : Option String
  materializeTriggerId : Option String
  materializeTriggerKind : Option String
  requestCausedById : Option String
  requestCausedByKind : Option String
  executionOrigin : Option String
  supersedeCallKeys : List String
  supersededPriorIds : List String
  targetNonterminalCountAfter : Option Nat
  requestCountAfter : Nat
  deriving Lean.ToJson

def dispatchOutput (scenario : TriggerScenario) : DispatchOutput :=
  let materialized := newRequest? scenario
  let fired := expectedResult scenario == "fired"
  { result := expectedResult scenario
    skipReason := expectedSkipReason scenario
    materializeTriggerId := if fired then scenario.intent.triggerId else none
    materializeTriggerKind := if fired then some scenario.intent.triggerKind.toDefraDB else none
    requestCausedById := materialized.bind causedById?
    requestCausedByKind := materialized.bind fun _ => causedByKind? scenario
    executionOrigin := materialized.map fun request => request.executionOrigin.toDefraDB
    supersedeCallKeys := expectedSupersedeCallKeys scenario
    supersededPriorIds := supersededPriorIds scenario
    targetNonterminalCountAfter := targetNonterminalCountAfter? scenario
    requestCountAfter := (after scenario).requests.length }

/-- What `TriggerEngine::dispatch` does with each intent: its result, the
request it materializes with that request's lineage, the supersede calls and
the target's non-terminal requests afterwards. -/
@[conformance_family]
def engineDispatch : Family DispatchInput DispatchOutput where
  inputs := triggerDispatchScenarios.map DispatchInput.ofScenario
  model := fun input => dispatchOutput input.scenario

/-- The dispatches the recent-runs view can project: it attributes only
requests a schedule or event trigger caused, so manual and skipped dispatches
leave it nothing to show. -/
def recentRunScenarios : List TriggerScenario :=
  triggerDispatchScenarios.filter fun scenario =>
    (dispatchOutput scenario).result == "fired" &&
      ((dispatchOutput scenario).requestCausedByKind == some "schedule" ||
        (dispatchOutput scenario).requestCausedByKind == some "event")

structure RecentRunOutput where
  requestCausedById : Option String
  requestCausedByKind : Option String
  executionOrigin : Option String
  deriving Lean.ToJson

/-- The lineage `task_run_histories` shows for a request a schedule or event
dispatch materialized. -/
@[conformance_family]
def recentRun : Family DispatchInput RecentRunOutput where
  inputs := recentRunScenarios.map DispatchInput.ofScenario
  model := fun input =>
    let output := dispatchOutput input.scenario
    { requestCausedById := output.requestCausedById
      requestCausedByKind := output.requestCausedByKind
      executionOrigin := output.executionOrigin }

/-- `gents automation task run` dispatches a manual intent with no trigger id
into an empty runtime, so the unconditional manual dispatch is the one case a
CLI run can build. -/
def manualRunScenarios : List TriggerScenario :=
  triggerDispatchScenarios.filter fun scenario => scenario.name == "manual_unconditional"

structure ManualRunOutput where
  result : String
  materializeTriggerId : Option String
  materializeTriggerKind : Option String
  executionOrigin : Option String
  deriving Lean.ToJson

/-- The request a CLI Task run materializes and the lineage it carries. -/
@[conformance_family]
def manualRun : Family DispatchInput ManualRunOutput where
  inputs := manualRunScenarios.map DispatchInput.ofScenario
  model := fun input =>
    let output := dispatchOutput input.scenario
    { result := output.result, materializeTriggerId := output.materializeTriggerId
      materializeTriggerKind := output.materializeTriggerKind
      executionOrigin := output.executionOrigin }

example : manualRunScenarios.map (fun scenario =>
    (scenario.intent.triggerId, scenario.intent.triggerKind.toDefraDB,
      concurrencyName scenario.intent.concurrency,
      (dispatchOutput scenario).requestCountAfter - scenario.before.requests.length)) =
    [(none, "manual", "parallel", 1)] := by native_decide

example : recentRunScenarios.any (fun scenario =>
    (dispatchOutput scenario).requestCausedByKind == some "schedule") &&
  recentRunScenarios.any (fun scenario =>
    (dispatchOutput scenario).requestCausedByKind == some "event") := by native_decide

end Conformance.TriggerContracts
