import Proofs.CompletionRetry.Properties
import Proofs.CanonicalOutput.Execution.Transition
import Proofs.CanonicalOutput.Execution.Properties
import Proofs.CanonicalOutput.Execution.Examples

namespace CompletionRetry.CanonicalExecution

open CanonicalOutput

def expectedCoordinate (purpose : RequestPurpose) (state : CompletionRetry.State) : Coordinate :=
  ⟨state.request, match purpose with
    | .normal => .provider state.scope state.turn state.attempt
    | .titleAudit => .auxiliary .title state.scope state.turn state.attempt⟩

def sourceMatches (purpose : RequestPurpose) (state : CompletionRetry.State)
    (closing : Segment) : Bool :=
  closing.coordinate == expectedCoordinate purpose state

end CompletionRetry.CanonicalExecution
