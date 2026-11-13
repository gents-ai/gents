import Proofs.Session.ProviderSanitation
import Proofs.Conformance.Family

namespace PromptAssembly.ProviderSanitation

private def user (id : Nat) : Row := ⟨"user", none, [⟨"text", id⟩]⟩
private def result (id : Nat) : Row := ⟨"user", some id, [⟨"text", id⟩]⟩
private def calls (ids : List Nat) : Row := ⟨"assistant", none, ids.map fun id => ⟨"call", id⟩⟩

private def assistantText (id : Nat) : Row := ⟨"assistant", none, [⟨"text", id⟩]⟩

private def uniqueCallCases : List Input :=
  [⟨[]⟩, ⟨[user 1]⟩, ⟨[result 1, user 2]⟩,
     ⟨[calls [1], result 1, user 2]⟩,
     ⟨[calls [1], user 2]⟩,
     ⟨[result 9, ⟨"assistant", none, [⟨"call", 1⟩, ⟨"call", 2⟩, ⟨"text", 3⟩]⟩, result 1, user 4]⟩,
     ⟨[user 1, calls [2], result 2, calls [3], result 3, user 4]⟩,
     ⟨[calls [1, 2], result 2, result 1, user 3]⟩,
     ⟨[calls [1, 2], result 1, user 3]⟩]

/-- On histories whose call IDs are unique the per-turn sanitizer agrees with
the global provider owner. -/
example : uniqueCallCases.map perTurnSanitize = uniqueCallCases.map sanitize := by native_decide

private def reusedKeyCases : List Input :=
  [⟨[calls [1, 1], result 1, result 1]⟩,
   ⟨[calls [1], user 2, calls [1], result 1]⟩,
   ⟨[calls [1], result 1, calls [1], result 1]⟩,
   ⟨[calls [1], result 1, calls [1]]⟩,
   ⟨[user 1, calls [1], result 1, assistantText 3, user 4, calls [1], result 1, assistantText 6]⟩]

@[conformance_family]
def sanitizeHistoryForProvider : Conformance.Family Input (List Row) :=
  ⟨uniqueCallCases ++ reusedKeyCases, perTurnSanitize⟩

@[conformance_family]
def safeToReduce : Conformance.Family Input Bool :=
  ⟨uniqueCallCases ++ reusedKeyCases ++
    [⟨[calls [1], result 1]⟩, ⟨[calls [1], assistantText 2]⟩,
     ⟨[result 1, user 2, assistantText 3]⟩, ⟨[user 1, assistantText 2]⟩],
    perTurnSafeToReduce⟩

end PromptAssembly.ProviderSanitation
