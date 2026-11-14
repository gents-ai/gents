import Proofs.ToolCalls.ToolDispatch
import Proofs.Shared.ToolExecution.Parallel
import Proofs.Conformance.Family

namespace ToolDispatch

deriving instance Lean.ToJson for Settlement, DeadlineDispatch

attribute [conformance_function] settleReady dispatchUnderDeadline

end ToolDispatch

namespace Conformance.ParallelDispatch

open Lean ToolExecution.Parallel

deriving instance ToJson for Admission, SettledCall, HookBatch

structure HookBatchInput where
  actions : List Admission
  hookCancelled : Bool
  deriving ToJson

@[conformance_family]
def hookDispatch : Family HookBatchInput HookBatch where
  inputs :=
    [[.run, .run], [.run, .skip, .run], [.skip], [.run, .terminate, .run], [.skip, .run, .terminate]].flatMap
      fun actions => [false, true].map fun hookCancelled => { actions, hookCancelled }
  model := fun i => ToolExecution.Parallel.hookBatch i.actions i.hookCancelled

end Conformance.ParallelDispatch
