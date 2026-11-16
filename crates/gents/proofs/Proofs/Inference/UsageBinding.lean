import Proofs.Inference.InferenceCall.LimitObservation

/-! A usage write names the credential row it was read for. The write lands only
while that row is still the one stored: a row replaced since the read is not the
row the write names, so the write is dropped. A write that names no row lands on
whichever row is current. -/

namespace UsageBinding

open InferenceCall.LimitObservation

def lands (observed : Option String) (current : String) : Bool :=
  observed.all fun bound => sameBinding bound current

theorem a_replaced_row_never_takes_the_write (bound current : String)
    (replaced : bound ≠ current) :
    lands (some bound) current = false := by
  simp [lands, sameBinding, replaced]

theorem a_write_naming_no_row_lands (current : String) :
    lands none current = true := rfl

theorem a_write_lands_on_the_row_it_names (bound : String) :
    lands (some bound) bound = true := by
  simp [lands, sameBinding]

end UsageBinding
