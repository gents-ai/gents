import Proofs.Session.PinnedRetryBudget
import Proofs.Conformance.Family

namespace Request.PinnedRetryBudget

@[conformance_family]
def configuredBudgets : Conformance.Family ConfiguredInput Outcome :=
  ⟨[none, some (-9223372036854775808), some (-1), some 0, some 1, some 3, some 7,
    some 9223372036854775807].map (⟨·⟩), resolveConfigured⟩

@[conformance_family]
def claimPins : Conformance.Family Input Outcome :=
  ⟨[ ⟨true, 3, false, none⟩, ⟨true, 0, false, none⟩, ⟨true, 7, false, some 2⟩
   , ⟨false, 3, false, none⟩, ⟨true, 5, true, none⟩, ⟨false, 5, true, none⟩
   , ⟨true, 5, true, some 0⟩, ⟨true, 5, true, some 2⟩, ⟨true, 1, true, some 9⟩
   , ⟨false, 1, true, some 4⟩
   , ⟨true, 3, true, some (-1)⟩
   , ⟨true, 3, true, some (-9223372036854775808)⟩
   , ⟨true, 3, true, some 9223372036854775807⟩ ], claimPin⟩

@[conformance_family]
def successorBudgets : Conformance.Family BudgetInput Bool :=
  ⟨[false, true].flatMap fun automated =>
    [ ⟨automated, 0, none⟩, ⟨automated, 2, none⟩, ⟨automated, 0, some 0⟩
    , ⟨automated, 0, some 3⟩, ⟨automated, 2, some 3⟩, ⟨automated, 3, some 3⟩
    , ⟨automated, 4, some 3⟩, ⟨automated, -1, some 3⟩
    , ⟨automated, 0, some (-1)⟩ ], successorBudgetRemains⟩

@[conformance_family]
def readPinsCases : Conformance.Family ReadInput (Option Int) :=
  ⟨[ [], [⟨true, 3⟩], [⟨false, 3⟩], [⟨false, 100⟩, ⟨true, 2⟩], [⟨true, 0⟩],
     [⟨true, 5⟩, ⟨true, 3⟩], [⟨true, 3⟩, ⟨false, 9223372036854775807⟩],
     [⟨false, 1⟩, ⟨false, 2⟩] ].map (⟨·⟩), readPins⟩

end Request.PinnedRetryBudget
