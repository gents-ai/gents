import Proofs.Tools.SelfConfig.ToolsRoot
import Proofs.Conformance.Family

open SelfConfig.ToolsRoot

namespace Conformance.SelfConfigToolsRoot

deriving instance Lean.ToJson for Operation, Outcome

structure SaveInput where
  operation : Operation
  authored : List String
  deriving Lean.ToJson

@[conformance_family]
def outcomes : Conformance.Family SaveInput Outcome where
  inputs :=
    [Operation.preview, .apply].flatMap fun operation =>
      [ ["selected", "detour", ".."], ["selected"], ["selected", "project"], ["sibling"],
        ["selected", "..", "sibling"] ].map fun authored => { operation, authored }
  model := fun i => save i.operation i.authored

end Conformance.SelfConfigToolsRoot
