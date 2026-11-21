import Proofs.Session.PromptAssembly.RequestContext
import Proofs.Conformance.ClaudePromptAssembly

namespace PromptAssembly.RequestContext

deriving instance Lean.ToJson for Section, HistoryReader

attribute [conformance_function] sections keepsStoredContext

end PromptAssembly.RequestContext
