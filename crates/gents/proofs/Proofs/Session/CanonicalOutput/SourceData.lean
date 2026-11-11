import Proofs.CanonicalOutput.State
import Mathlib.Data.List.Dedup

namespace CanonicalOutput.Execution

def sourceData (records : List Segment) (coordinate : Coordinate) : List Segment :=
  (sourceRecords records coordinate).filter (fun record => record.flush.isSome) |>.dedup

end CanonicalOutput.Execution
