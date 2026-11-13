import Proofs.Session.Recovery.Sweeps.Requests
import Proofs.ToolCalls.Recovery.Sweeps.ToolCalls
import Proofs.ToolCalls.Recovery.Sweeps.SessionMessage
import Proofs.Inference.Recovery.Sweeps.Inference
import Proofs.ToolCalls.Recovery.Sweeps.TaskHooks
import Proofs.ToolCalls.Recovery.Sweeps.PluginJob
import Proofs.Session.Recovery.Sweeps.OwedDelivery

namespace Recovery

def registeredRecoverySweeps : List RecoverySweep :=
  [ requestRecoverySweep
  , toolCallRecoverySweep
  , orphanedBackgroundToolSweep
  , backgroundCompletionSideEffectSweep
  , terminalParentOwnedToolSweep
  , sessionMessageRecoverySweep
  , inferenceCallRecoverySweep
  , taskHookRecoverySweep
  , pluginJobRecoverySweep
  , owedDeliveryRecoverySweep
  ]

def registeredRecoverySweepIds : List String :=
  registeredRecoverySweeps.map fun sweep => sweep.sweepId

def registeredRecoverySweepContracts : List (String × String) :=
  registeredRecoverySweeps.map fun sweep =>
    (sweep.sweepId, sweep.collection.toContract)

theorem registered_sweeps_cover_persisted_collections :
    ∀ collection,
      collection ∈ PersistedRecoveryCollection.all →
      ∃ sweep,
        sweep ∈ registeredRecoverySweeps ∧
        sweep.collection = collection := by
  intro collection _h_collection
  cases collection with
  | agentRequest =>
      exact ⟨requestRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩
  | agentToolCall =>
      exact ⟨toolCallRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩
  | inferenceCall =>
      exact ⟨inferenceCallRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩
  | taskHookRecord =>
      exact ⟨taskHookRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩
  | owedDelivery =>
      exact ⟨owedDeliveryRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩

/-- The sweeps one periodic tick runs, in the order the runtime runs them. This
is not the registry order above: the tick runs the inference sweep before the
session-message sweep. -/
def periodicRecoveryTick : List RecoverySweep :=
  [ requestRecoverySweep
  , terminalParentOwnedToolSweep
  , orphanedBackgroundToolSweep
  , backgroundCompletionSideEffectSweep
  , inferenceCallRecoverySweep
  , sessionMessageRecoverySweep
  , taskHookRecoverySweep
  , pluginJobRecoverySweep
  , owedDeliveryRecoverySweep
  ]

def periodicRecoveryTickPosition (sweepId : String) : Option Nat :=
  periodicRecoveryTick.findIdx? fun sweep => sweep.sweepId == sweepId

def periodicRecoveryTickFunction (sweepId : String) : Option String :=
  (periodicRecoveryTick.find? fun sweep => sweep.sweepId == sweepId).map fun sweep =>
    sweep.rustFunction

def runsAfterSessionMessageSweep (sweepId : String) : Bool :=
  match periodicRecoveryTickPosition sweepId,
      periodicRecoveryTickPosition "tool_call_lifecycle_recover_session_message_rows" with
  | some position, some sessionPosition => decide (sessionPosition < position)
  | _, _ => false

theorem plugin_job_sweep_is_registered_after_the_session_message_sweep :
    runsAfterSessionMessageSweep "tool_call_lifecycle_recover_plugin_job_rows" = true := by
  native_decide

end Recovery
