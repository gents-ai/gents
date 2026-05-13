import Proofs.MCPHealth.Transition
import Proofs.MCPHealth.Coupling

/-!
# MCP Health — Conformance witnesses

Exhaustive enumeration of transitions across
`K ∈ {1, 2, 3} × startState ∈ HealthState.all × startCount ∈ {0..K} ×
event ∈ Event.all`. Each row is evaluated by `step?` and tagged with the
Rust 3-state projection.

`k1ProjectionCases` is the subset Rust consumes today (K=1, matching the
current `health_checker.rs` behavior). The K ≥ 2 rows are emitted but not
yet asserted by any Rust test — they form the formal contract for a future
K-aware refactor.
-/

namespace Proofs.MCPHealth

/-- Witness row for a single transition. -/
structure TransitionCase where
  name           : String
  startState     : HealthState
  startCount     : Nat
  event          : Event
  thresholdK     : Nat
  nextState      : Option HealthState   -- none = service removed
  nextCount      : Option Nat
  rustProjection : Option String         -- "healthy" | "stale" | "unreachable" | none
  deriving Repr

namespace TransitionCase

/-- Build a row by applying `step?` to (startState, startCount, event, K). -/
def build (startState : HealthState) (startCount : Nat) (event : Event)
    (thresholdK : Nat) (hk : thresholdK ≥ 1) : TransitionCase :=
  let K : Threshold := Threshold.ofNat thresholdK hk
  let sm : ServiceModel := { state := startState, failureCount := startCount }
  let next := step? sm event K
  let nameSuffix := match next with
    | none => "removed"
    | some sm' => sm'.state.toDefraDB ++ "_" ++ toString sm'.failureCount
  { name :=
      "mcp_health_K" ++ toString thresholdK ++ "_"
        ++ startState.toDefraDB ++ "_" ++ toString startCount ++ "_"
        ++ event.toDefraDB ++ "_" ++ nameSuffix
  , startState := startState
  , startCount := startCount
  , event := event
  , thresholdK := thresholdK
  , nextState := next.map (·.state)
  , nextCount := next.map (·.failureCount)
  , rustProjection :=
      next.map fun sm' => (healthProjection sm'.state).toDefraDB
  }

end TransitionCase

/-- Range `[0..K)` of valid starting `failureCount` values for a given K.
    `0` for staleness-degraded / Healthy / Reconnecting; up to `K-1` for
    failure-count-degraded; `K` only appears as a *next* count, not a start. -/
def countRange (K : Nat) : List Nat :=
  List.range K  -- [0, 1, ..., K-1]

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

/-- K=1 subset — the rows Rust witnesses today. -/
def k1ProjectionCases : List TransitionCase :=
  transitionCases.filter (·.thresholdK = 1)

/-- K ≥ 2 subset — emitted but not yet asserted by any Rust test. -/
def k2PlusFutureCases : List TransitionCase :=
  transitionCases.filter (·.thresholdK ≥ 2)

/-- `k1ProjectionCases` and `k2PlusFutureCases` partition `transitionCases`. -/
theorem k1_k2_partition :
    k1ProjectionCases.length + k2PlusFutureCases.length = transitionCases.length := by
  native_decide

end Proofs.MCPHealth
