import Proofs.Store.Identity.UserDelegation
import Proofs.Conformance.Family

namespace Identity.UserDelegation

deriving instance Lean.ToJson for Carrier, Write, Carried, Scope, Verdict, Author, Admission, Credential,
  Reported, Resolution, Presented, Edge

attribute [conformance_function] admit credential runtimeSignsWithHeldUserKey statusReports resolve
  edge

structure MintTimeInput where
  providerAtRead : Option Nat
  clockAtRead : Nat
  clock : Nat
  deriving Lean.ToJson

structure MintTimeOutput where
  issueAt : Nat
  generalLifetime : Nat
  deriving Lean.ToJson

def MintTimeInput.home (input : MintTimeInput) : Option HomeClock :=
  input.providerAtRead.map (⟨·, input.clockAtRead⟩)

/-- A client that read no clock, one behind its home, one ahead, and one whose
clock was set back since it read. -/
@[conformance_family]
def mintTimes : Conformance.Family MintTimeInput MintTimeOutput where
  inputs :=
    [ ⟨none, 0, 1_000⟩, ⟨some 1_300, 1_000, 1_010⟩, ⟨some 1_000, 1_300, 1_310⟩,
      ⟨some 1_000, 1_300, 1_200⟩ ]
  model := fun input =>
    { issueAt := issueAt input.home input.clock
      generalLifetime := generalLifetime input.home input.clock }

end Identity.UserDelegation
