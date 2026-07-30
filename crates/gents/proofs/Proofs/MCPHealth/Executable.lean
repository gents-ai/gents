import Proofs.MCPHealth.Transition
import Proofs.MCPHealth.Coupling

namespace Proofs.MCPHealth

structure TransitionCase where
  name           : String
  startState     : HealthState
  startCount     : Nat
  event          : Event
  thresholdK     : Nat
  nextState      : Option HealthState
  nextCount      : Option Nat
  rustProjection : Option String
  deriving Repr

namespace TransitionCase

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

def countRange (K : Nat) : List Nat :=
  List.range K

def transitionCasesFor (K : Nat) (hk : K ≥ 1) : List TransitionCase :=
  HealthState.all.flatMap fun s =>
    (countRange K).flatMap fun n =>
      Event.all.map fun e =>
        TransitionCase.build s n e K hk

def transitionCases : List TransitionCase :=
  transitionCasesFor 1 (by decide) ++
  transitionCasesFor 2 (by decide) ++
  transitionCasesFor 3 (by decide)

def k1ProjectionCases : List TransitionCase :=
  transitionCases.filter (·.thresholdK = 1)

def k2PlusFutureCases : List TransitionCase :=
  transitionCases.filter (·.thresholdK ≥ 2)

theorem k1_k2_partition :
    k1ProjectionCases.length + k2PlusFutureCases.length = transitionCases.length := by
  native_decide

end Proofs.MCPHealth
