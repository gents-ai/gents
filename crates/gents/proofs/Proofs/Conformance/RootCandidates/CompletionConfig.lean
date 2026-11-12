import Proofs.Runtime.CompletionConfig
import Proofs.Conformance.Family

namespace Runtime.CompletionConfig

@[conformance_family]
def loopConfigForRequest : Conformance.Family Input (Option Observation) :=
  ⟨["interactive", "scheduled", "unknown"].flatMap (fun origin =>
    [none, some (-1), some 0, some 2].flatMap fun interactive =>
      [none, some [100, 0, -1, 200]].flatMap fun delays =>
        [none, some 1893456000].map fun deadline =>
          ⟨origin, none, interactive, none, none, delays, 2, deadline⟩), resolve⟩

end Runtime.CompletionConfig
