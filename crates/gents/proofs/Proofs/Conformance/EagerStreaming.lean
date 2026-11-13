import Proofs.Inference.EagerStreaming
import Proofs.Conformance.Family

namespace EagerStreaming

deriving instance Lean.ToJson for Provider

attribute [conformance_function] enabled

structure BedrockToolInput where
  defaultEndpoint : Bool
  flagged : Bool
  deriving Lean.ToJson

@[conformance_family]
def bedrockToolStreaming : Conformance.Family BedrockToolInput Bool where
  inputs := [false, true].flatMap fun defaultEndpoint =>
    [false, true].map fun flagged => { defaultEndpoint, flagged }
  model := fun i => enabled .bedrock i.defaultEndpoint i.flagged false

end EagerStreaming
