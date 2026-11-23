import Proofs.Store.PluginStoreHome
import Proofs.Conformance.Family

namespace PluginStoreHome

deriving instance Lean.ToJson for Outcome
deriving instance Lean.ToJson for Writer
deriving instance Lean.ToJson for WriteOutcome

attribute [conformance_function] acquire
attribute [conformance_function] write

end PluginStoreHome
