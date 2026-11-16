import Proofs.Session.ManagedExec
import Proofs.Session.ManagedExec.Composed
import Proofs.ToolCalls.Background.ProcessControl
import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

structure ManagedExecLivenessCase where
  name : String
  trigger : ManagedExec.Trigger
  preExecState : String
  preToolState : String
  deriving Repr

def managedExecLivenessCase (name : String) (trigger : ManagedExec.Trigger) :
    ManagedExecLivenessCase :=
  { name, trigger
  , preExecState := ManagedExecState.toDefraDB .running
  , preToolState := ToolExecution.ToolCallState.toDefraDB .running }

def managedExecLivenessCases : List ManagedExecLivenessCase :=
  [ managedExecLivenessCase "running_child_expired_deadline_kill_signaled" .deadlineElapsed
  , managedExecLivenessCase "running_child_cancel_kill_signaled" .cancelRequested
  , managedExecLivenessCase "fast_child_exit_completes_without_kill" .observeExitSuccess
  , managedExecLivenessCase "nonzero_child_exit_fails_without_kill" .observeExitFailure
  , managedExecLivenessCase "deadline_timeout_preserves_partial_stdout_metadata" .deadlineElapsed
  ]

def processStopCases : List ProcessStopCase :=
  (ManagedExec.ProcessObservation.all.flatMap fun before =>
    ManagedExec.ProcessObservation.all.map fun after =>
      let outcome := ManagedExec.stopOutcome before after
      { name := "process_" ++ before.toContract ++ "_then_" ++ after.toContract
      , before := before.toContract
      , after := after.toContract
      , mayTerminate := ManagedExec.mayTerminate before
      , retainRecord := ManagedExec.retainProcessRecord after
      , outcome := outcome.toContract
      , cancelReply := (ManagedExec.cancelReply outcome).toContract
      , durableState := (Background.ProcessControl.stopSettlement outcome).toDefraDB
      }) ++
  ([false, true].flatMap fun released =>
    [("never_spawned", Background.ProcessControl.LiveProcessEvidence.neverSpawned),
     ("spawned_unverified", Background.ProcessControl.LiveProcessEvidence.spawnedUnverified),
     ("exited", Background.ProcessControl.LiveProcessEvidence.exited)].map fun (name, evidence) =>
      let outcome := Background.ProcessControl.settleLiveCancel released evidence
      let after : ManagedExec.ProcessObservation := if outcome == .stopped then .exited else .running
      { name := "live_" ++ name ++ "_" ++ toString released
      , liveEvidence := name
      , workerReleased := released
      , settleCompleted := Background.ProcessControl.maySettleTerminal .completed evidence
      , settleCancelled := Background.ProcessControl.maySettleTerminal .cancelled evidence
      , settleTimedOut := Background.ProcessControl.maySettleTerminal .timedOut evidence
      , settleFailed := Background.ProcessControl.maySettleTerminal .failed evidence
      , settlePanicked := Background.ProcessControl.maySettleTerminal .panicked evidence
      , before := "running"
      , after := after.toContract
      , mayTerminate := true
      , retainRecord := ManagedExec.retainProcessRecord after
      , outcome := outcome.toContract
      , cancelReply := (ManagedExec.cancelReply outcome).toContract
      , durableState := (Background.ProcessControl.stopSettlement outcome).toDefraDB
      })

theorem processStopCases_cancelled_only_after_observed_stop :
    ∀ witness ∈ processStopCases,
      witness.cancelReply = "cancelled" →
        witness.before = "running" ∧ witness.after = "exited" := by
  native_decide

end Conformance.ContractCases
