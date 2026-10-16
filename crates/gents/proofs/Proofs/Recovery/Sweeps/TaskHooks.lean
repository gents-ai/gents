import Proofs.Recovery.Contract

namespace Recovery

/-- One host task hook record: the attempt observations an execution wrote
    before each hook launched, which `TaskHooks.recoverInterrupted` selects
    remaining cleanup from. It is host state beside the runtime's locked store,
    never a replicated document, because a hook's effects happened on this
    host. `requestResolved` holds once the request's terminal is written (by
    its owner or request recovery) or the request is gone; `liveExecution`
    holds while an executor in this runtime still owns the record. -/
structure TaskHookRecordRow where
  recorded : Bool
  requestResolved : Bool
  liveExecution : Bool
  deriving DecidableEq, Repr

/-- Cleanup recovery waits for the request's terminal: running cleanup while
    the request is still owned would race the executor that owes it, and
    request recovery is what decides the terminal cleanup must not rewrite. -/
def taskHookRecordStale (row : TaskHookRecordRow) : Prop :=
  row.recorded = true ∧ row.requestResolved = true ∧ row.liveExecution = false

instance (row : TaskHookRecordRow) : Decidable (taskHookRecordStale row) := by
  unfold taskHookRecordStale
  infer_instance

/-- Remaining cleanup runs once, then the record is forgotten. -/
def taskHookRecordRecover (row : TaskHookRecordRow) : TaskHookRecordRow :=
  if taskHookRecordStale row then { row with recorded := false } else row

def taskHookRecordMeasure (row : TaskHookRecordRow) : Nat :=
  if taskHookRecordStale row then 1 else 0

theorem taskHookRecord_stale_positive :
    ∀ row, taskHookRecordStale row → taskHookRecordMeasure row > 0 := by
  intro row h
  simp [taskHookRecordMeasure, h]

theorem taskHookRecord_recover_released :
    ∀ row, taskHookRecordStale row → (taskHookRecordRecover row).recorded = false := by
  intro row h
  simp [taskHookRecordRecover, h]

theorem taskHookRecord_recover_zero :
    ∀ row, taskHookRecordStale row →
      taskHookRecordMeasure (taskHookRecordRecover row) = 0 := by
  intro row h
  have h_released := taskHookRecord_recover_released row h
  have h_not : ¬ taskHookRecordStale (taskHookRecordRecover row) := by
    intro h_stale
    rw [h_stale.1] at h_released
    exact Bool.noConfusion h_released
  simp [taskHookRecordMeasure, h_not]

/-- A record whose request is still owned, or that a live executor holds, is
    left untouched. -/
theorem taskHookRecord_not_stale_unchanged (row : TaskHookRecordRow)
    (h : ¬ taskHookRecordStale row) : taskHookRecordRecover row = row := by
  simp [taskHookRecordRecover, h]

def taskHookRecoverySweep : RecoverySweep :=
  { Row := TaskHookRecordRow
  , collection := .taskHookRecord
  , sweepId := "task_hook_recover_interrupted_cleanup"
  , rustFunction := "task_hooks::recover_task_hook_records"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := taskHookRecordStale
  , recover := taskHookRecordRecover
  , terminal := fun row => row.recorded = false
  , measure := taskHookRecordMeasure
  , h_stale_positive := taskHookRecord_stale_positive
  , h_recover_terminal := taskHookRecord_recover_released
  , h_recover_zero := taskHookRecord_recover_zero
  }

end Recovery
