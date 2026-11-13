import Proofs.Session.ToolExecution.ArgumentFailure
import Proofs.Conformance.Family

open PromptAssembly.ToolArgumentFailure ToolExecution.ArgumentFailure

namespace Conformance.ArgumentFailure

deriving instance Lean.ToJson for ParseFailure, FailureKind

structure RefusalInput where
  failure : ParseFailure
  deriving Lean.ToJson

structure RefusalOutput where
  notice : FailureKind
  namesField : Bool
  lifecycleState : String
  failureClass : Option String
  endsRequest : Bool
  deriving Lean.ToJson

/-- A complete response: a provider stream that ends normally reports no token
limit. -/
@[conformance_family]
def refusal : Conformance.Family RefusalInput RefusalOutput where
  inputs := [.syntax, .unknownField, .missingField, .wrongType].map fun failure => { failure }
  model := fun i =>
    let kind := classify .unreported i.failure
    { notice := kind, namesField := namesField kind, lifecycleState := recordedState,
      failureClass := recordedClass, endsRequest := endsRequest }

end Conformance.ArgumentFailure
