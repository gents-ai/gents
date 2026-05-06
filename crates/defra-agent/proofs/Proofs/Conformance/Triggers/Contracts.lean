import Proofs.Conformance.Triggers.Trace

/-!
# Trigger Dispatch Conformance Contracts

Finite executable trigger scenarios emitted for Rust conformance tests.
The cases are computed from `dispatch` and `dispatchStep`, so Rust can
exercise the same branch matrix without hand-maintaining expected outcomes.
-/

namespace Conformance.TriggerContracts

structure TriggerScenario where
  name : String
  snap : TriggerSnapshot
  before : SystemState
  intent : FireIntent

def concurrencyName : ConcurrencyMode → String
  | .parallel => "parallel"
  | .serial => "serial"
  | .latestOnly => "latest_only"

def jsonString (s : String) : String :=
  "\"" ++ s ++ "\""

def jsonArray (values : List String) : String :=
  "[" ++ String.intercalate "," values ++ "]"

def jsonStringArray (values : List String) : String :=
  jsonArray (values.map jsonString)

def jsonOptionString : Option String → String
  | none => "null"
  | some value => jsonString value

def jsonOptionNat : Option Nat → String
  | none => "null"
  | some value => toString value

def keyJson (key : TriggerKey) : String :=
  "{"
    ++ "\"trigger_id\":" ++ jsonString key.1 ++ ","
    ++ "\"trigger_kind\":" ++ jsonString key.2.toDefraDB
    ++ "}"

def schedule (triggerId : String) : ActiveSchedule :=
  { triggerId := triggerId, enabled := true }

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
  , causedBy := some (triggerId, triggerKind)
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
  scenario.intent.triggerId.map fun triggerId =>
    (triggerId, scenario.intent.triggerKind)

def newRequest? (scenario : TriggerScenario) : Option AgentRequest :=
  (after scenario).requests[scenario.before.requests.length]?

def causedById? (request : AgentRequest) : Option String :=
  request.causedBy.map Prod.fst

def causedByKind? (request : AgentRequest) : Option String :=
  request.causedBy.map fun key => key.2.toDefraDB

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
  | some _, .latestOnly, some triggerId => [(triggerId, scenario.intent.triggerKind)]
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
      ++ (materialized.map causedById? |>.join |> jsonOptionString) ++ ","
    ++ "\"expected_request_caused_by_kind\":"
      ++ (materialized.map causedByKind? |>.join |> jsonOptionString) ++ ","
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
  , { name := "schedule_serial_same_tuple_skips"
    , snap := snapshot ["sched-a"] []
    , before := { requests := [request "prior-schedule" "sched-a" .schedule .serial false] }
    , intent := intent (some "sched-a") .schedule .serial
    }
  , { name := "schedule_serial_same_id_other_kind_fires"
    , snap := snapshot ["shared"] []
    , before := { requests := [request "prior-event" "shared" .event .serial false] }
    , intent := intent (some "shared") .schedule .serial
    }
  , { name := "event_serial_same_tuple_skips"
    , snap := snapshot [] ["event-a"]
    , before := { requests := [request "prior-event" "event-a" .event .serial false] }
    , intent := intent (some "event-a") .event .serial
    }
  , { name := "schedule_latest_only_supersedes_prior"
    , snap := snapshot ["sched-a"] []
    , before := { requests := [request "prior-schedule" "sched-a" .schedule .latestOnly false] }
    , intent := intent (some "sched-a") .schedule .latestOnly
    }
  , { name := "manual_latest_only_without_key_fires_without_supersede"
    , snap := snapshot [] []
    , before := { requests := [request "prior-schedule" "sched-a" .schedule .latestOnly false] }
    , intent := intent none .manual .latestOnly
    }
  ]

def triggerDispatchCasesJson : String :=
  jsonArray (triggerDispatchScenarios.map contractJson)

end Conformance.TriggerContracts
