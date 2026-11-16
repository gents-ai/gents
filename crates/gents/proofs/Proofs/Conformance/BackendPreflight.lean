import Proofs.Store.BackendPreflight
import Proofs.Conformance.Family

namespace Conformance.BackendPreflight

structure CopyInput where
  result : _root_.BackendPreflight.CopyResult
  deriving Lean.ToJson

@[conformance_family]
def copyCheck : Family CopyInput String where
  inputs := [.compatible, .incompatible, .unavailable].map fun result => { result }
  model := fun i => _root_.BackendPreflight.decision i.result

end Conformance.BackendPreflight
