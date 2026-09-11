import Proofs.Triggers.Groups
import Proofs.Conformance.ContractTypes

namespace Conformance.EventGroupContracts

open Conformance.Contracts
open Triggers
open Triggers.Groups

structure GroupScenario where
  name : String
  before : MarkerState
  candidate : Candidate

def key (did correlation : String) : EventGroupKey :=
  { agentDid := did
  , consumer := .trigger "event-a"
  , consumerConfigKey := "config-1"
  , correlation := correlation
  }

def candidate
    (did correlation : String)
    (actual : Nat)
    (expected : Option Nat)
    (minimum : Nat := 1)
    (timedOut : Bool := false)
    (wellFormed : Bool := true) : Candidate :=
  { key := key did correlation
  , actualCount := actual
  , expectedCount := expected
  , minimumCount := minimum
  , timedOut := timedOut
  , wellFormed := wellFormed
  }

def marker (did correlation : String) : MarkerState :=
  { materialized := [key did correlation] }

def scenarios : List GroupScenario :=
  [ { name := "complete_unmarked_materializes"
    , before := { materialized := [] }
    , candidate := candidate "did:agent:a" "run-1" 3 (some 3)
    }
  , { name := "incomplete_does_not_materialize"
    , before := { materialized := [] }
    , candidate := candidate "did:agent:a" "run-1" 2 (some 3)
    }
  , { name := "overfull_does_not_materialize"
    , before := { materialized := [] }
    , candidate := candidate "did:agent:a" "run-1" 4 (some 3)
    }
  , { name := "timeout_at_floor_materializes"
    , before := { materialized := [] }
    , candidate := candidate "did:agent:a" "run-1" 2 none 2 true
    }
  , { name := "counted_partial_timeout_at_floor_materializes"
    , before := { materialized := [] }
    , candidate := candidate "did:agent:a" "run-1" 2 (some 3) 2 true
    }
  , { name := "timeout_below_floor_does_not_materialize"
    , before := { materialized := [] }
    , candidate := candidate "did:agent:a" "run-1" 1 none 2 true
    }
  , { name := "malformed_does_not_materialize"
    , before := { materialized := [] }
    , candidate := candidate "did:agent:a" "run-1" 3 (some 3) 1 false false
    }
  , { name := "existing_full_key_marker_suppresses"
    , before := marker "did:agent:a" "run-1"
    , candidate := candidate "did:agent:a" "run-1" 3 (some 3)
    }
  , { name := "different_owner_does_not_suppress"
    , before := marker "did:agent:b" "run-1"
    , candidate := candidate "did:agent:a" "run-1" 3 (some 3)
    }
  , { name := "different_correlation_does_not_suppress"
    , before := marker "did:agent:a" "run-2"
    , candidate := candidate "did:agent:a" "run-1" 3 (some 3)
    }
  , { name := "callback_and_trigger_same_id_do_not_alias"
    , before := { materialized := [{ (key "did:agent:a" "run-1") with
        consumer := .callbackBinding "event-a" }] }
    , candidate := candidate "did:agent:a" "run-1" 3 (some 3)
    }
  , { name := "changed_consumer_config_does_not_suppress"
    , before := { materialized := [{ (key "did:agent:a" "run-1") with
        consumerConfigKey := "config-2" }] }
    , candidate := candidate "did:agent:a" "run-1" 3 (some 3)
    }
  , { name := "callback_group_materializes_with_common_eligibility"
    , before := { materialized := [] }
    , candidate := { (candidate "did:agent:a" "run-1" 3 (some 3)) with
        key := { (key "did:agent:a" "run-1") with consumer := .callbackBinding "event-a" } }
    }
  ]

def jsonOptionNat : Option Nat → String
  | none => "null"
  | some value => toString value

def consumerJson : EventConsumer → String
  | .trigger id => "{\"kind\":\"trigger\",\"trigger_id\":" ++ jsonString id ++ "}"
  | .callbackBinding id =>
      "{\"kind\":\"callback_binding\",\"binding_id\":" ++ jsonString id ++ "}"

def keyJson (value : EventGroupKey) : String :=
  "{"
    ++ "\"agent_did\":" ++ jsonString value.agentDid ++ ","
    ++ "\"consumer\":" ++ consumerJson value.consumer ++ ","
    ++ "\"consumer_config_key\":" ++ jsonString value.consumerConfigKey ++ ","
    ++ "\"correlation\":" ++ jsonString value.correlation
    ++ "}"

def scenarioJson (scenario : GroupScenario) : String :=
  let after := reconcile scenario.before scenario.candidate
  "{"
    ++ "\"name\":" ++ jsonString scenario.name ++ ","
    ++ "\"candidate\":" ++ keyJson scenario.candidate.key ++ ","
    ++ "\"actual_count\":" ++ toString scenario.candidate.actualCount ++ ","
    ++ "\"expected_count\":" ++ jsonOptionNat scenario.candidate.expectedCount ++ ","
    ++ "\"minimum_count\":" ++ toString scenario.candidate.minimumCount ++ ","
    ++ "\"timed_out\":" ++ toString scenario.candidate.timedOut ++ ","
    ++ "\"well_formed\":" ++ toString scenario.candidate.wellFormed ++ ","
    ++ "\"prior_markers\":" ++ jsonArray (scenario.before.materialized.map keyJson) ++ ","
    ++ "\"eligible\":" ++ toString scenario.candidate.eligible ++ ","
    ++ "\"materialized\":" ++ toString (after.has scenario.candidate.key) ++ ","
    ++ "\"marker_count_after\":" ++ toString after.materialized.length
    ++ "}"

def eventGroupCaseCount : Nat := scenarios.length

def eventGroupCasesJson : String := jsonArray (scenarios.map scenarioJson)

end Conformance.EventGroupContracts
