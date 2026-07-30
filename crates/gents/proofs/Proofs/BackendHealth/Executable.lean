import Proofs.BackendHealth.Transition

namespace Proofs.BackendHealth

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

theorem transition_cases_blocks_routing_sound :
    transitionCases.all (fun c => c.blocksRouting = c.nextState.blocksRouting) := by
  native_decide

end Proofs.BackendHealth
