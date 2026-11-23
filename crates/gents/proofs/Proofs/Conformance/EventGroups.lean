import Proofs.Automation.Triggers.Groups
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.EventGroupContracts

open Conformance.Contracts
open Triggers
open Triggers.Groups

structure GroupScenario where
  name : String
  before : MarkerState
  candidate : Candidate

def key (did correlation : String) : EventGroupKey :=
  { nodeDid := did
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

structure ConsumerWire where
  kind : String
  triggerId : Option String
  bindingId : Option String
  deriving Lean.ToJson

def ConsumerWire.ofModel : EventConsumer → ConsumerWire
  | .trigger id => ⟨"trigger", some id, none⟩
  | .callbackBinding id => ⟨"callback_binding", none, some id⟩

structure KeyWire where
  nodeDid : String
  consumer : ConsumerWire
  consumerConfigKey : String
  correlation : String
  deriving Lean.ToJson

def KeyWire.ofModel (value : EventGroupKey) : KeyWire :=
  ⟨value.nodeDid, ConsumerWire.ofModel value.consumer, value.consumerConfigKey, value.correlation⟩

structure EligibilityInput where
  name : String
  candidate : KeyWire
  actualCount : Nat
  expectedCount : Option Nat
  minimumCount : Nat
  timedOut : Bool
  wellFormed : Bool
  deriving Lean.ToJson

structure EligibilityOutput where
  eligible : Bool
  deriving Lean.ToJson

/-- Whether `group_candidate_eligible` lets a gathered event group materialize,
over every group scenario's candidate. -/
@[conformance_family]
def eligibility : Family EligibilityInput EligibilityOutput where
  inputs := scenarios.map fun scenario =>
    { name := scenario.name, candidate := KeyWire.ofModel scenario.candidate.key
      actualCount := scenario.candidate.actualCount
      expectedCount := scenario.candidate.expectedCount
      minimumCount := scenario.candidate.minimumCount
      timedOut := scenario.candidate.timedOut, wellFormed := scenario.candidate.wellFormed }
  model := fun i =>
    { eligible := ({ key := key "" "", actualCount := i.actualCount, expectedCount := i.expectedCount
                     minimumCount := i.minimumCount, timedOut := i.timedOut
                     wellFormed := i.wellFormed } : Candidate).eligible }

end Conformance.EventGroupContracts
