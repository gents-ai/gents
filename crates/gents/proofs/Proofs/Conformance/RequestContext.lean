import Proofs.Session.PromptAssembly.RequestContext
import Proofs.Conformance.ClaudePromptAssembly
import Proofs.Session.SessionFork

namespace PromptAssembly.RequestContext

deriving instance Lean.ToJson for Section, HistoryReader

attribute [conformance_function] sections keepsStoredContext

end PromptAssembly.RequestContext

namespace Conformance.RequestContext

open Lean

structure KeyInput where
  key : String
  deriving ToJson

@[conformance_family]
def runtimeContextKey : Family KeyInput Bool where
  inputs := [
      "authored:bae-1:context", "authored:bae-1:prompt", "authored::context",
      "authored:bae-1:context:extra", "authored:bae-1:output-obligation:0", "assistant:bae-1:0",
      "context", "authored:a:b:context", "AUTHORED:bae-1:context", "authored:bae-1:Context"
    ].map fun key => { key }
  model := fun i => SessionFork.runtimeContextKey i.key

end Conformance.RequestContext
