import Proofs.Store.StoreMigration
import Proofs.Conformance.Family

namespace StoreMigration

deriving instance Lean.ToJson for BackendPreflight.Lineage

attribute [conformance_function] openDisposition changeRule migrationStep mayFinish policyTransition

end StoreMigration
