import Proofs.ToolCalls.Background.Interrupt
import Proofs.ToolCalls.Recovery.Sweeps.ToolCalls

/-! Interrupting a background row's caller, followed by the periodic orphan
sweep. The sweep's scope is the production `ToolRowKind::in_orphan_sweep_scope`
(background native rows); a plugin-job row is outside it, so its caller's
interrupt and the sweep together leave the job running. -/

namespace Recovery

open ToolExecution

/-- Lean counterpart of `ToolRowKind::in_orphan_sweep_scope`: the periodic
orphan sweep selects background native rows and no other row. -/
def orphanSweepSelects (call : ToolCallContext) : Bool :=
  decide (call.awaitMode = .background) && decide (RowClass.of call = .native)

theorem orphanSweep_never_selects_plugin_job (call : ToolCallContext)
    (h_job : call.operation = .pluginJob) :
    orphanSweepSelects call = false := by
  simp [orphanSweepSelects, RowClass.of, h_job]

/-- The orphan-sweep row of a background tool whose caller was interrupted:
its parent is interrupted, no host process is registered, and the row is
the interrupted call. -/
def callerInterruptedRow (c : ToolCallContext) : OrphanedBackgroundToolRow :=
  { call := Background.Interrupt.interruptTool c
  , deadlineExpired := false
  , parentLive := false
  , parentInterrupted := true
  , parentTerminal := false
  , executionRegistered := false
  , process := .stopped
  , ownerTaskDeleted := false }

/-- The periodic orphan sweep terminalizes the row: it selects the row and the
selected row has a cause. -/
def orphanSweepTerminalizes (c : ToolCallContext) : Bool :=
  orphanSweepSelects (Background.Interrupt.interruptTool c) &&
    (orphanedBackgroundToolCause (callerInterruptedRow c)).isSome

/-- Interrupting the caller of a running background plugin job leaves the job
running, and the orphan sweep neither selects nor terminalizes it. -/
theorem interrupted_caller_keeps_its_job (c : ToolCallContext)
    (h_job : c.operation = .pluginJob)
    (h_running : c.state = .running)
    (h_background : c.awaitMode = .background) :
    orphanSweepSelects (Background.Interrupt.interruptTool c) = false ∧
      (Background.Interrupt.interruptTool c).state = .running := by
  obtain ⟨h_same, _⟩ :=
    Background.Interrupt.interrupt_retains_running_detached_job c h_job h_running h_background
  rw [h_same]
  exact ⟨orphanSweep_never_selects_plugin_job c h_job, h_running⟩

theorem interrupted_caller_job_is_not_terminalized (c : ToolCallContext)
    (h_job : c.operation = .pluginJob)
    (h_running : c.state = .running)
    (h_background : c.awaitMode = .background) :
    orphanSweepTerminalizes c = false := by
  have h := (interrupted_caller_keeps_its_job c h_job h_running h_background).1
  simp [orphanSweepTerminalizes, h]

end Recovery
