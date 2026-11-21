import Proofs.Store.PluginStoreHome
import Proofs.Conformance.Family

namespace PluginStoreHome

deriving instance Lean.ToJson for Outcome

attribute [conformance_function] acquire

end PluginStoreHome
