import Proofs.Automation.Triggers.ScheduleOccurrence
import Proofs.Conformance.Family

namespace Conformance.ScheduleOccurrence

open Lean Triggers.ScheduleOccurrence

structure DispatchInput where
  dispatches : List String
  deriving ToJson

structure OccurrenceIdentity where
  requestId : String
  sessionId : String
  retryKey : String
  deriving ToJson

structure DispatchOutput where
  requests : Nat
  replayed : List Bool
  answers : List Nat
  identities : List OccurrenceIdentity
  deriving ToJson

def position (x : String) : List String → Nat
  | [] => 0
  | y :: ys => if x = y then 0 else position x ys + 1

/-- Dispatches of a plain schedule Trigger's occurrences, in order, through the
production materializer: how many requests exist after them, whether each
dispatch answered with a request an earlier one published, and which published
request (by publication order) each answered. `identities` is each dispatched
occurrence's identity for the placeholder owner `node` and Trigger `trigger`;
the consumer maps the placeholders to its own. -/
@[conformance_family]
def dispatches : Family DispatchInput DispatchOutput where
  inputs :=
    let a := "2030-01-01T00:00:00Z"
    let b := "2030-01-01T00:01:00Z"
    [[a], [a, a], [a, b], [a, a, b, a], [b, a, b]].map fun dispatches => { dispatches }
  model := fun i =>
    let (store, outcomes) := dispatchOutcomes "node" "trigger" [] i.dispatches
    let published := outcomes.filterMap fun outcome =>
      if outcome.isReplay then none else some outcome.requestId
    { requests := store.length
      replayed := outcomes.map Outcome.isReplay
      answers := outcomes.map fun outcome => position outcome.requestId published
      identities := i.dispatches.map fun occurrence =>
        { requestId := requestId "node" "trigger" occurrence
          sessionId := sessionId "node" "trigger" occurrence
          retryKey := retryKey "node" "trigger" occurrence } }

end Conformance.ScheduleOccurrence
