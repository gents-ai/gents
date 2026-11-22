import Proofs.Store.DecisionRead
import Proofs.Conformance.Family

namespace DecisionRead

deriving instance Lean.ToJson for AccessCheck, Outcome, Step

attribute [conformance_function] step

end DecisionRead

namespace Conformance.DecisionRead

open Lean

structure CodeInput where
  code : String
  deriving ToJson

/-- DefraDB's two access check codes, the codes beside them, and codes that
only resemble them. -/
@[conformance_family]
def accessCheck : Family CodeInput (Option _root_.DecisionRead.AccessCheck) where
  inputs := ["ACCESS_CHECK_UNAVAILABLE", "ACCESS_CHECK_FAILED", "PERMISSION_DENIED",
    "TXN_CONFLICT", "STORAGE_FULL", "", "access_check_unavailable",
    "ACCESS_CHECK_UNAVAILABLE "].map fun code => { code }
  model := fun i => _root_.DecisionRead.accessCheck i.code

end Conformance.DecisionRead
