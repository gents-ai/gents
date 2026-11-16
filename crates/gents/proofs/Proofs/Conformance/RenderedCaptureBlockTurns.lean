import Proofs.Shared.RenderedCapture
import Proofs.Conformance.Family

namespace Conformance.RenderedCaptureBlockTurns

open RenderedCapture

deriving instance Lean.ToJson for BlockState, TurnOutcome

def blockStates : List BlockState := [.healthy, .wrongPayload, .absent]

structure BlockTurnInput where
  first : BlockState
  second : BlockState
  third : BlockState
  deriving Lean.ToJson

structure BlockTurnOutput where
  first : TurnOutcome
  second : TurnOutcome
  third : TurnOutcome
  deriving Lean.ToJson

@[conformance_family]
def sequence : Conformance.Family BlockTurnInput BlockTurnOutput where
  inputs :=
    blockStates.flatMap fun first => blockStates.flatMap fun second =>
      blockStates.map fun third => { first, second, third }
  model := fun i =>
    { first := (sequenceOutcomes i.first i.second i.third).1
      second := (sequenceOutcomes i.first i.second i.third).2.1
      third := (sequenceOutcomes i.first i.second i.third).2.2 }

end Conformance.RenderedCaptureBlockTurns
