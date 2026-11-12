import Proofs.Session.ProviderSanitation
import Proofs.Conformance.Family

namespace PromptAssembly.ProviderSanitation

private def user (id : Nat) : Row := ⟨"user", none, [⟨"text", id⟩]⟩
private def result (id : Nat) : Row := ⟨"user", some id, [⟨"text", id⟩]⟩
private def calls (ids : List Nat) : Row := ⟨"assistant", none, ids.map fun id => ⟨"call", id⟩⟩

@[conformance_family]
def sanitizeHistoryForProvider : Conformance.Family Input (List Row) :=
  ⟨[⟨[]⟩, ⟨[user 1]⟩, ⟨[result 1, user 2]⟩,
     ⟨[calls [1], result 1, user 2]⟩,
     ⟨[calls [1], user 2]⟩,
     ⟨[result 9, ⟨"assistant", none, [⟨"call", 1⟩, ⟨"call", 2⟩, ⟨"text", 3⟩]⟩, result 1, user 4]⟩,
     ⟨[user 1, calls [2], result 2, calls [3], result 3, user 4]⟩,
     ⟨[calls [1, 2], result 2, result 1, user 3]⟩,
     ⟨[calls [1, 2], result 1, user 3]⟩], sanitize⟩

end PromptAssembly.ProviderSanitation
