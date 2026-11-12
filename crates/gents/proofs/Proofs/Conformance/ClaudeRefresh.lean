import Proofs.Inference.ClaudeRefresh
import Proofs.Conformance.Family

namespace Conformance.ClaudeRefresh

open Lean RefreshClassification

deriving instance ToJson for RefreshClass

structure RefreshInput where
  status : Nat
  errorCode : Option String
  deriving ToJson

@[conformance_family]
def failureClass : Family RefreshInput RefreshClass where
  inputs := [400, 401, 403, 429, 500].flatMap fun status =>
    [none, some "invalid_grant", some "invalid_scope", some "invalid_token",
      some "forbidden", some "invalid_request"].map fun errorCode => { status, errorCode }
  model := fun i => refreshClass i.status i.errorCode

end Conformance.ClaudeRefresh
