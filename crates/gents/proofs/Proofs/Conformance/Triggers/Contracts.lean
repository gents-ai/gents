import Proofs.Conformance.Triggers.Trace
import Proofs.Conformance.ContractTypes

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

/-- Lineage source comes from the selected seed independently of gate identity. -/
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
      | .parallel | .latestOnly => none

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

def contractJson (scenario : TriggerScenario) : String :=
  let materialized := newRequest? scenario
  "{"
    ++ "\"name\":" ++ jsonString scenario.name ++ ","
    ++ "\"trigger_id\":" ++ jsonOptionString scenario.intent.triggerId ++ ","
    ++ "\"trigger_kind\":" ++ jsonString scenario.intent.triggerKind.toDefraDB ++ ","
    ++ "\"intent_task_id\":" ++ jsonString scenario.intent.taskId ++ ","
    ++ "\"selected_task_id\":"
      ++ jsonOptionString ((dispatch scenario.snap scenario.intent).map RequestSeed.taskId) ++ ","
    ++ "\"concurrency\":" ++ jsonString (concurrencyName scenario.intent.concurrency) ++ ","
    ++ "\"active_schedule_ids\":"
      ++ jsonStringArray (scenario.snap.activeSchedules.map ActiveSchedule.triggerId) ++ ","
    ++ "\"active_event_trigger_ids\":"
      ++ jsonStringArray (scenario.snap.activeEventTriggers.map ActiveEventTrigger.triggerId) ++ ","
    ++ "\"prior_nonterminal_keys\":"
      ++ jsonArray (priorNonterminalKeys scenario |>.map keyJson) ++ ","
    ++ "\"expected_result\":" ++ jsonString (expectedResult scenario) ++ ","
    ++ "\"expected_skip_reason\":" ++ jsonOptionString (expectedSkipReason scenario) ++ ","
    ++ "\"expected_materialize_trigger_id\":"
      ++ (if expectedResult scenario = "fired" then
            jsonOptionString scenario.intent.triggerId
          else
            "null") ++ ","
    ++ "\"expected_materialize_trigger_kind\":"
      ++ (if expectedResult scenario = "fired" then
            jsonOptionString (some scenario.intent.triggerKind.toDefraDB)
          else
            "null") ++ ","
    ++ "\"expected_request_caused_by_id\":"
      ++ (materialized.bind causedById? |> jsonOptionString) ++ ","
    ++ "\"expected_request_caused_by_kind\":"
      ++ (materialized.bind (fun _ => causedByKind? scenario) |> jsonOptionString) ++ ","
    ++ "\"expected_execution_origin\":"
      ++ (materialized.map (fun request => request.executionOrigin.toDefraDB)
          |> jsonOptionString) ++ ","
    ++ "\"expected_supersede_call_keys\":"
      ++ jsonArray (expectedSupersedeCallKeys scenario |>.map keyJson) ++ ","
    ++ "\"superseded_prior_ids\":"
      ++ jsonStringArray (supersededPriorIds scenario) ++ ","
    ++ "\"target_nonterminal_count_after\":"
      ++ jsonOptionNat (targetNonterminalCountAfter? scenario) ++ ","
    ++ "\"request_count_before\":" ++ toString scenario.before.requests.length ++ ","
    ++ "\"request_count_after\":" ++ toString (after scenario).requests.length
    ++ "}"

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

def triggerDispatchCaseCount : Nat :=
  triggerDispatchScenarios.length

def triggerDispatchCasesJson : String :=
  jsonArray (triggerDispatchScenarios.map contractJson)

end Conformance.TriggerContracts
