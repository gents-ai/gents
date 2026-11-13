import Proofs.Runtime.ReasoningCapabilities
import Proofs.Conformance.Family

namespace Runtime.ReasoningCapabilities

@[conformance_family]
def authoritativeReasoningCapabilities : Conformance.Family Input (Option String) :=
  ⟨[false, true].flatMap (fun messages =>
    [none, some "{}", some "{\"_gents_advertised_reasoning_efforts\":[\"high\"],\"top_k\":40}"].flatMap fun params =>
      [none, some [], some ["low", "xhigh"]].map fun advertised =>
        ⟨messages, params, advertised⟩), authoritative⟩

end Runtime.ReasoningCapabilities
