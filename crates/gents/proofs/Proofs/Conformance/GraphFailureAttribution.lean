import Proofs.GraphPipeline.FailureAttribution
import Proofs.Conformance.ContractTypes

namespace Conformance.GraphFailureAttributionContracts

open GraphPipeline GraphPipeline.FailureAttribution Conformance.Contracts

structure Observation where
  status : RunStatus
  cancellationRequested : Bool
  generation : Nat
  primary : Option Cause
  mayInterruptForFailure : Bool
  deriving DecidableEq, Repr

def observe (s : Snapshot) : Observation :=
  ⟨s.run.status, s.run.cancellationRequested, s.generation, s.primary,
    GraphPipeline.FailureAttribution.mayInterruptForFailure s⟩

structure TraceCase where
  name : String
  events : List Event
  expected : List Observation
  deriving DecidableEq, Repr

/-- Explicit expected rows, independent of replay. Cause IDs are identities,
not priority ranks. Rust must map actions to the existing GraphRun owner and
read the persisted row after each action; this is not a Rust reference machine. -/
def traceCases : List TraceCase :=
  [ { name := "earlier_interrupted_sibling_does_not_replace_cause"
      events := [.capture 0 (some 90), .capture 1 (some 10), .finish 1 true (some 10)]
      expected := [⟨.running, false, 1, some 90, true⟩,
                   ⟨.running, false, 1, some 90, true⟩, ⟨.failed, false, 2, some 90, false⟩] }
  , { name := "reversed_lexical_order_keeps_cause"
      -- Explicitly introduce the second physical failure before recapture.
      events := [.capture 0 (some 10), .observedFailure 90,
                 .capture 1 (some 10), .finish 1 true (some 10)]
      expected := [⟨.running, false, 1, some 10, true⟩,
                   ⟨.running, false, 1, some 10, true⟩,
                   ⟨.running, false, 1, some 10, true⟩, ⟨.failed, false, 2, some 10, false⟩] }
  , { name := "concurrent_stale_capture_loses"
      events := [.capture 0 (some 90), .capture 0 (some 10), .finish 1 true (some 10)]
      expected := [⟨.running, false, 1, some 90, true⟩,
                   ⟨.running, false, 1, some 90, true⟩, ⟨.failed, false, 2, some 90, false⟩] }
  , { name := "restart_after_capture_reuses_durable_cause"
      events := [.capture 0 (some 90), .capture 1 (some 90), .finish 1 true (some 10)]
      expected := [⟨.running, false, 1, some 90, true⟩,
                   ⟨.running, false, 1, some 90, true⟩, ⟨.failed, false, 2, some 90, false⟩] }
  , { name := "active_sibling_requires_drain"
      events := [.capture 0 (some 90), .finish 1 false (some 10), .finish 1 true (some 10)]
      expected := [⟨.running, false, 1, some 90, true⟩,
                   ⟨.running, false, 1, some 90, true⟩, ⟨.failed, false, 2, some 90, false⟩] }
  , { name := "cancel_after_capture_wins"
      events := [.capture 0 (some 90), .cancel, .capture 2 (some 10), .finish 2 true (some 10)]
      expected := [⟨.running, false, 1, some 90, true⟩,
                   ⟨.running, true, 2, some 90, false⟩, ⟨.running, true, 2, some 90, false⟩,
                   ⟨.cancelled, true, 3, none, false⟩] }
  , { name := "cancel_before_capture_blocks_failure"
      events := [.cancel, .capture 1 (some 90), .finish 1 true (some 90)]
      expected := [⟨.running, true, 1, none, false⟩,
                   ⟨.running, true, 1, none, false⟩, ⟨.cancelled, true, 2, none, false⟩] }
  , { name := "direct_failure_without_active_siblings"
      events := [.finish 0 true (some 90)]
      expected := [⟨.failed, false, 1, some 90, false⟩] }
  , { name := "no_witness_cannot_install_failure"
      events := [.capture 0 none, .finish 0 true none]
      expected := [⟨.running, false, 0, none, false⟩, ⟨.running, false, 0, none, false⟩] }
  , { name := "terminal_retry_is_noop"
      events := [.capture 0 (some 90), .finish 1 true (some 10),
                 .capture 2 (some 10), .cancel, .finish 2 true (some 10)]
      expected := [⟨.running, false, 1, some 90, true⟩, ⟨.failed, false, 2, some 90, false⟩,
                   ⟨.failed, false, 2, some 90, false⟩, ⟨.failed, false, 2, some 90, false⟩,
                   ⟨.failed, false, 2, some 90, false⟩] }
  ]

theorem all_traces_replay_explicit_expectations :
    ∀ c ∈ traceCases, (trace initial c.events).map observe = c.expected := by decide

theorem traceCases_count : traceCases.length = 10 := by decide

private def boolJson (b : Bool) : String := if b then "true" else "false"
private def causeJson : Option Cause → String
  | none => "null"
  | some n => toString n
private def statusJson : RunStatus → String
  | .running => jsonString "running"
  | .succeeded => jsonString "succeeded"
  | .failed => jsonString "failed"
  | .cancelled => jsonString "cancelled"

def eventJson : Event → String
  | .observedFailure cause =>
      "{\"kind\":\"observed_failure\",\"witness\":" ++ causeJson (some cause) ++ "}"
  | .capture expected witness =>
      "{\"kind\":\"capture\",\"expected_generation\":" ++ toString expected ++
      ",\"witness\":" ++ causeJson witness ++ "}"
  | .cancel => "{\"kind\":\"cancel\"}"
  | .finish expected allTerminal witness =>
      "{\"kind\":\"finish\",\"expected_generation\":" ++ toString expected ++
      ",\"all_terminal\":" ++ boolJson allTerminal ++ ",\"witness\":" ++ causeJson witness ++ "}"

def observationJson (o : Observation) : String :=
  "{\"status\":" ++ statusJson o.status ++
  ",\"cancellation_requested\":" ++ boolJson o.cancellationRequested ++
  ",\"generation\":" ++ toString o.generation ++ ",\"primary\":" ++ causeJson o.primary ++
  ",\"may_interrupt_for_failure\":" ++ boolJson o.mayInterruptForFailure ++ "}"

def traceCaseJson (c : TraceCase) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"initial\":" ++ observationJson (observe initial) ++
  ",\"events\":" ++ jsonArray (c.events.map eventJson) ++
  ",\"expected\":" ++ jsonArray (c.expected.map observationJson) ++ "}"

def traceCasesJson : String := jsonArray (traceCases.map traceCaseJson)

end Conformance.GraphFailureAttributionContracts
