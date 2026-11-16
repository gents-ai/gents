import Proofs.Store.StoreEncryptionUpgrade
import Proofs.Conformance.Family

namespace StoreEncryptionUpgrade

deriving instance Lean.ToJson for Phase, Action

attribute [conformance_function] next afterRecheck mayFinish

end StoreEncryptionUpgrade
