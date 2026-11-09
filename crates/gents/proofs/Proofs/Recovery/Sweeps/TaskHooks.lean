import Proofs.Recovery.Contract

namespace Recovery








/-- A native ACP read can hide an existing request. Absence from an Agent's
view, a missing signer, or a failed read cannot prove terminality. -/
inductive TaskHookRequestObservation where
  | terminal
  | active
  | hidden
  | unavailable
  deriving DecidableEq, Repr

structure TaskHookRecordRow where
  recorded : Bool
  requestObservation : TaskHookRequestObservation
  liveExecution : Bool
  deriving DecidableEq, Repr

def taskHookRecordStale (row : TaskHookRecordRow) : Prop :=
  row.recorded = true ∧ row.requestObservation = .terminal ∧ row.liveExecution = false

instance (row : TaskHookRecordRow) : Decidable (taskHookRecordStale row) := by
  unfold taskHookRecordStale
  infer_instance

theorem taskHookRecord_hidden_not_stale (row : TaskHookRecordRow)
    (h : row.requestObservation = .hidden) : ¬ taskHookRecordStale row := by
  simp [taskHookRecordStale, h]

theorem taskHookRecord_unavailable_not_stale (row : TaskHookRecordRow)
    (h : row.requestObservation = .unavailable) : ¬ taskHookRecordStale row := by
  simp [taskHookRecordStale, h]

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
