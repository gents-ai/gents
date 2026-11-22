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

namespace ToolExecution.Parallel

deriving instance Lean.ToJson for Footprint, Call

attribute [conformance_function] joinsGroup

structure ScriptBatch where
  calls : List Call
  deriving Lean.ToJson

/-- Every batch of up to six calls by footprint with distinct keys, and the
same batch with its second call repeating its first where their footprints
agree; six calls exceed the bound. -/
@[conformance_family]
def scriptEffectWidth : Conformance.Family ScriptBatch Nat where
  inputs := (List.range 7).flatMap fun len => (List.range (2 ^ len)).flatMap fun mask =>
    let calls := (List.range len).map fun i =>
      (⟨i, if mask.testBit i then .barrier else .read⟩ : Call)
    ⟨calls⟩ :: match calls with
      | a :: b :: rest =>
        if a.footprint = b.footprint then [⟨a :: { b with key := a.key } :: rest⟩] else []
      | _ => []
  model := fun batch => effectWidth maxParallelReads batch.calls

end ToolExecution.Parallel
