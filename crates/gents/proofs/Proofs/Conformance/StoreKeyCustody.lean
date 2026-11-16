import Proofs.Store.StoreKeyCustody
import Proofs.Conformance.Family

namespace StoreKeyCustody

deriving instance Lean.ToJson for Outcome, KeyFile, Step

attribute [conformance_function] prepare resume load

end StoreKeyCustody
