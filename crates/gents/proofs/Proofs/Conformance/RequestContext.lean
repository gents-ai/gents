import Proofs.Session.PromptAssembly.RequestContext
import Proofs.Conformance.Identity.AgentCreationReceipt

namespace PromptAssembly.RequestContext

deriving instance Lean.ToJson for Section

attribute [conformance_function] sections

end PromptAssembly.RequestContext
