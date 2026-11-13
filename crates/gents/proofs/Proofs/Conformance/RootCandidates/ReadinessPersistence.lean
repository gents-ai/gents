import Proofs.Runtime.ReadinessPersistence
import Proofs.Conformance.Family

namespace Runtime.ReadinessPersistence

@[conformance_family]
def commitCandidates : Conformance.Family Input (List Observation) :=
  ⟨["fail", "timeout", "full"].flatMap (fun failure =>
    [0, 1, 4, 5].map fun count =>
      ⟨[⟨"recovering", List.replicate count failure ++ ["ok"], false⟩,
        ⟨"ready", ["ok"], false⟩, ⟨"shutdown", ["ok"], false⟩]⟩) ++
    [⟨[⟨"recovering", ["ok"], false⟩, ⟨"ready", ["timeout"], true⟩]⟩,
     ⟨[⟨"recovering", ["ok"], false⟩, ⟨"recovering", [], false⟩]⟩], run⟩

end Runtime.ReadinessPersistence
