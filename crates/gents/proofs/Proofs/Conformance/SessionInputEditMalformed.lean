import Proofs.Session.Identity.SessionInputEdit
import Proofs.Conformance.Family

namespace Conformance.SessionInputEditMalformed

open Lean Identity.SessionInputEdit

structure IntentInput where
  intentDecodes : Bool
  deriving ToJson

structure IntentOutput where
  unansweredSelected : Bool
  processingFails : Bool
  deriving ToJson

/-- The command is written with no receipt, so the unanswered sweep sees it.
The decodable case is a valid command from the same fixture; the undecodable
case is an intent that is not JSON. -/
@[conformance_family]
def intent : Family IntentInput IntentOutput where
  inputs := [{ intentDecodes := false }, { intentDecodes := true }]
  model := fun i =>
    { unansweredSelected := unansweredSelected false
      processingFails := processingFails i.intentDecodes }

end Conformance.SessionInputEditMalformed
