import Proofs.Shared.CompletionParametersMerge
import Proofs.Conformance.Family

namespace CompletionParameters.Merge

private def parameterSets : List (Option String) :=
  [none, some "null", some "{}", some "[]", some "1",
   some "{\"provider\":{\"require_parameters\":true}}",
   some "{\"top_p\":0.95,\"top_k\":40}",
   some "{\"provider\":{\"require_parameters\":false,\"order\":[\"a\"]}}"]

@[conformance_family]
def optionalParams : Conformance.Family Input (Option String) :=
  ⟨parameterSets.flatMap (fun left => parameterSets.map fun right => ⟨left, right⟩), merge⟩

end CompletionParameters.Merge
