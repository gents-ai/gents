import Proofs.BackendHealth.Transition

/-!
# Backend Health — Conformance witnesses

Exhaustive enumeration of transitions across
`K ∈ {1, 2, 3} × startState ∈ HealthState.all × startCount ∈ {0..K-1} ×
event ∈ Event.all`. Each row is evaluated by `step` (total — no removal),
and tagged with the `blocksRouting` projection of the next state so the
Rust consumer fences the routing veto, not just the state label.

K=3 is the production default (`BackendProberOptions::failure_threshold_k`);
K ∈ {1, 2} rows fence the machine's shape across the configurable range.
-/

namespace Proofs.BackendHealth

/-- Witness row for a single transition. -/
structure TransitionCase where
  name          : String
  startState    : HealthState
  startCount    : Nat
  event         : Event
  thresholdK    : Nat
  nextState     : HealthState
  nextCount     : Nat
  blocksRouting : Bool
  deriving Repr

namespace TransitionCase

/-- Build a row by applying `step` to (startState, startCount, event, K). -/
def build (startState : HealthState) (startCount : Nat) (event : Event)
    (thresholdK : Nat) (hk : thresholdK ≥ 1) : TransitionCase :=
  let K : Threshold := Threshold.ofNat thresholdK hk
  let m : Model := { state := startState, failureCount := startCount }
  let next := step m event K
  { name :=
      "backend_health_K" ++ toString thresholdK ++ "_"
        ++ startState.toDefraDB ++ "_" ++ toString startCount ++ "_"
        ++ event.toDefraDB ++ "_"
        ++ next.state.toDefraDB ++ "_" ++ toString next.failureCount
  , startState := startState
  , startCount := startCount
  , event := event
  , thresholdK := thresholdK
  , nextState := next.state
  , nextCount := next.failureCount
  , blocksRouting := next.state.blocksRouting }

end TransitionCase

/-- Valid starting `failureCount` values `[0..K-1]`: `K` only appears as a
    *next* count (a success resets before the counter can exceed K). -/
def countRange (K : Nat) : List Nat :=
  List.range K

/-- Generate all rows for a single K. -/
def transitionCasesFor (K : Nat) (hk : K ≥ 1) : List TransitionCase :=
  HealthState.all.flatMap fun s =>
    (countRange K).flatMap fun n =>
      Event.all.map fun e =>
        TransitionCase.build s n e K hk

/-- All conformance rows for K ∈ {1, 2, 3}. -/
def transitionCases : List TransitionCase :=
  transitionCasesFor 1 (by decide) ++
  transitionCasesFor 2 (by decide) ++
  transitionCasesFor 3 (by decide)

/-- Every emitted row's routing veto agrees with `blocksRouting` of its next
    state — the JSON consumer can trust the flag without re-deriving it. -/
theorem transition_cases_blocks_routing_sound :
    transitionCases.all (fun c => c.blocksRouting = c.nextState.blocksRouting) := by
  native_decide

end Proofs.BackendHealth
