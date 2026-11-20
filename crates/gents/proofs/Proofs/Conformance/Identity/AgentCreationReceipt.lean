import Proofs.Session.Identity.AgentCreationReceipt
import Proofs.Conformance.Family

namespace Conformance.AgentCreationReceipt

open Lean Identity.AgentCreationReceipt

deriving instance ToJson for Cause

structure OpeningInput where
  signatureValid : Bool
  scopeMatches : Bool
  cause : Cause
  expectedOpening : Option String
  signedOpening : String
  deriving ToJson

@[conformance_family]
def opening : Family OpeningInput Bool where
  inputs :=
    [true, false].flatMap fun signatureValid =>
      [true, false].flatMap fun scopeMatches =>
        [Cause.agentNew, .taskTrigger, .pluginJob].flatMap fun cause =>
          [none, some "first-opening", some "second-opening"].map fun expectedOpening =>
            { signatureValid, scopeMatches, cause, expectedOpening, signedOpening := "first-opening" }
  model := fun i => verifies i.signatureValid i.scopeMatches i.cause i.expectedOpening i.signedOpening

end Conformance.AgentCreationReceipt
