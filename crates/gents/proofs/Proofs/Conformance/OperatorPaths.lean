import Proofs.Tools.ToolPolicy.OperatorPaths
import Proofs.Conformance.Family
import Lean

namespace Conformance.OperatorPaths
open Lean

inductive PathKind where
  | ordinary | ordinaryAlias | writeOnly | missing | named | uncreatedNamed | «alias»
  | queueAlias | staged | stagedAlias | uncreatedStaged | unavailable | guardUnavailable
  deriving ToJson

/-- Whether the target is a named operator path, and whether it is the same
filesystem object as an operator object (`none` when inspection fails). -/
def PathKind.facts : PathKind → Bool × Option Bool
  | .ordinary => (false, some false)
  | .ordinaryAlias => (false, some false)
  | .writeOnly => (false, some false)
  | .missing => (false, some false)
  | .named => (true, some true)
  | .uncreatedNamed => (true, some false)
  | .«alias» => (false, some true)
  | .queueAlias => (false, some true)
  | .staged => (true, some true)
  | .stagedAlias => (false, some true)
  | .uncreatedStaged => (true, some false)
  | .unavailable => (false, none)
  | .guardUnavailable => (false, none)

structure PathInput where
  kind : PathKind
  deriving ToJson

@[conformance_family]
def protection : Family PathInput Bool where
  inputs := [PathKind.ordinary, .ordinaryAlias, .writeOnly, .missing, .named, .uncreatedNamed,
    .«alias», .queueAlias, .staged, .stagedAlias, .uncreatedStaged, .unavailable,
    .guardUnavailable].map fun kind => { kind }
  model := fun i => ToolPolicy.OperatorPaths.isProtected i.kind.facts.1 i.kind.facts.2

structure CommandCase where
  required : Bool
  supported : Bool
  inspected : Bool
  singleLinked : Bool
  deriving ToJson

/-- The command guard, as distinct from the path kinds. -/
inductive GuardKind where
  | commandGuard
  deriving ToJson

structure CommandInput where
  kind : GuardKind
  command : CommandCase
  deriving ToJson

@[conformance_family]
def commandGuard : Family CommandInput Bool where
  inputs := [false, true].flatMap fun required => [false, true].flatMap fun supported =>
    [false, true].flatMap fun inspected => [false, true].map fun singleLinked =>
      { kind := .commandGuard, command := { required, supported, inspected, singleLinked } }
  model := fun i => ToolPolicy.OperatorPaths.commandGuardAllows i.command.required
    i.command.supported i.command.inspected i.command.singleLinked

end Conformance.OperatorPaths
