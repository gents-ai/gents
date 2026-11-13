import Proofs.Shared.CompletionRetry.ErrorIdentity
import Proofs.Conformance.Family

open CompletionRetry.RepeatedToolFailure CompletionRetry.ErrorIdentity

namespace Conformance.RepeatedFailureIdentity

deriving instance Lean.ToJson for Action, Ending

structure StreakInput where
  commandEnvelope : Bool
  varyingHint : Bool
  calls : Nat
  deriving Lean.ToJson

structure StreakOutput where
  actions : List Action
  ending : Option Ending
  deriving Lean.ToJson

@[conformance_family]
def streak : Conformance.Family StreakInput StreakOutput where
  inputs :=
    [false, true].flatMap fun commandEnvelope =>
      [false, true].flatMap fun varyingHint =>
        [3, 5].map fun calls => { commandEnvelope, varyingHint, calls }
  model := fun i =>
    let (actions, ending) := run {} (failures i.commandEnvelope i.varyingHint i.calls)
    { actions, ending }

end Conformance.RepeatedFailureIdentity
