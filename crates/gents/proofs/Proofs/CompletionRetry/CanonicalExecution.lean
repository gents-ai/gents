import Proofs.CompletionRetry.Properties
import Proofs.CanonicalOutput.Execution.Transition
import Proofs.CanonicalOutput.Execution.Properties
import Proofs.CanonicalOutput.Execution.Examples

/-!
# Retry / canonical source alignment

These helpers bind retry policy to the exact canonical provider coordinate.
The application transition itself lives in `CompletionRetry.CanonicalGate`, so
there is no gate-bypassing paired-world wrapper or second mutation path here.
-/
namespace CompletionRetry.CanonicalExecution

open CanonicalOutput

def expectedCoordinate (state : CompletionRetry.State) : Coordinate :=
  ⟨state.request, .provider state.scope state.turn state.attempt⟩

def sourceMatches (state : CompletionRetry.State) (closing : Segment) : Bool :=
  closing.coordinate == expectedCoordinate state

end CompletionRetry.CanonicalExecution
