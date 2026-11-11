import Proofs.Shared.CompletionRetry.Properties
import Proofs.Runtime.CanonicalOutput.Execution.Transition
import Proofs.Runtime.CanonicalOutput.Execution.Properties
import Proofs.Runtime.CanonicalOutput.Execution.Examples

namespace CompletionRetry.CanonicalExecution

open CanonicalOutput

def expectedCoordinate (purpose : RequestPurpose) (state : CompletionRetry.State) : Coordinate :=
  ⟨state.request, match purpose with
    | .normal => .provider state.scope state.turn state.attempt
    | .titleAudit => .auxiliary .title state.scope state.turn state.attempt
    | .accessReview => .auxiliary .accessReview state.scope state.turn state.attempt⟩

def sourceMatches (purpose : RequestPurpose) (state : CompletionRetry.State)
    (closing : Segment) : Bool :=
  closing.coordinate == expectedCoordinate purpose state

end CompletionRetry.CanonicalExecution
