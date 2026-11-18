import Proofs.Store.Identity.HeldAudience
import Proofs.Conformance.Family
import Proofs.Conformance.UserDelegation
import Proofs.Conformance.TransactionReplay

namespace Identity.HeldAudience

deriving instance Lean.ToJson for Held, Change, Refusal, Answer, Told, UnitStage

attribute [conformance_function] change resend refusalOf told resendUnit rereadBeforeRequestBound
  reportedOfHome

end Identity.HeldAudience

namespace Identity.HeldAudience

structure ClockReading where
  providerAtRead : Nat
  clockAtRead : Nat
  deriving Lean.ToJson

structure ClockReadInput where
  held : Option ClockReading
  read : Option ClockReading
  deriving Lean.ToJson

def ClockReading.clock : Option ClockReading → Option UserDelegation.HomeClock :=
  Option.map fun reading => ⟨reading.providerAtRead, reading.clockAtRead⟩

/-- A first read, a read reporting no clock, reads within the allowance either
way, and reads past it either way. -/
@[conformance_family]
def clockMoves : Conformance.Family ClockReadInput Bool where
  inputs :=
    [ ⟨none, some ⟨1_000, 900⟩⟩, ⟨some ⟨1_000, 900⟩, none⟩, ⟨none, none⟩,
      ⟨some ⟨1_000, 900⟩, some ⟨1_130, 1_000⟩⟩, ⟨some ⟨1_000, 900⟩, some ⟨1_070, 1_000⟩⟩,
      ⟨some ⟨1_000, 900⟩, some ⟨1_131, 1_000⟩⟩, ⟨some ⟨1_000, 900⟩, some ⟨1_069, 1_000⟩⟩ ]
  model := fun input => clockMoved (ClockReading.clock input.held) (ClockReading.clock input.read)

end Identity.HeldAudience
