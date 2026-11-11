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

end Recovery
