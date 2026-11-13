import Proofs.Tools.ToolPolicy.OperatorPaths
import Proofs.Conformance.Family

namespace Conformance.OperatorProtection

open Lean
open ToolPolicy.OperatorPaths

deriving instance ToJson for Guard, MountEntry, MountVerdict
deriving instance ToJson for ProtectionStep, ProtectionOutcome

structure MountInput where
  rootExists : Bool
  guard : Guard
  entries : List MountEntry
  deriving ToJson

structure CommandTrace where
  steps : List ProtectionStep
  deriving ToJson

@[conformance_family]
def mount : Family MountInput MountVerdict where
  inputs :=
    [ ⟨true, .outside, [.ordinary]⟩
    , ⟨true, .outside, [.ordinary, .sharesOperatorObject]⟩
    , ⟨true, .namedWithin, []⟩
    , ⟨true, .uninspectable, []⟩
    , ⟨false, .unguarded, []⟩
    , ⟨true, .unguarded, [.sharesOperatorObject]⟩ ]
  model := fun input => mountVerdict input.rootExists input.guard input.entries

@[conformance_family]
def command : Family CommandTrace (List ProtectionOutcome) where
  inputs :=
    [ ⟨[.capture, .addAlias, .revalidate, .capture, .removeAlias, .revalidate]⟩
    , ⟨[.addAlias, .capture, .removeAlias, .capture]⟩ ]
  model := fun trace => runProtection false trace.steps

end Conformance.OperatorProtection
