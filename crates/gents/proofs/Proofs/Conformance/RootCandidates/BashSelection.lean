import Proofs.Tools.BashSelection
import Proofs.Conformance.Family

namespace Tools.BashSelection

@[conformance_family]
def documentSelection : Conformance.Family Input Observation :=
  ⟨["off", "read_only", "unrestricted"].flatMap (fun mode =>
    [none, some "read_only", some "workspace_write", some "unrestricted"].flatMap fun execution =>
      [none, some [], some ["cat", "journalctl"]].flatMap fun allowlist =>
        [false, true].map fun background => ⟨mode, execution, allowlist, background⟩), select⟩

end Tools.BashSelection
