import Proofs.Recovery.Sweeps.Requests
import Proofs.Recovery.Sweeps.ToolCalls
import Proofs.Recovery.Sweeps.SessionMessage
import Proofs.Recovery.Sweeps.Inference

namespace Recovery

def registeredRecoverySweeps : List RecoverySweep :=
  [ requestRecoverySweep
  , toolCallRecoverySweep
  , orphanedBackgroundToolSweep
  , backgroundCompletionSideEffectSweep
  , terminalParentOwnedToolSweep
  , sessionMessageRecoverySweep
  , inferenceCallRecoverySweep
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

end Recovery
