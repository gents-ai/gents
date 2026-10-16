import Proofs.ApplyReconcile.RuntimeBridge
import Proofs.ApplyReconcile.Publication

namespace ApplyReconcile

/-- Accepted publication realizes every desired field before the same canonical
registry decoder and readiness projection run. -/
theorem publication_resolved_snapshot
    (M : Manifest) (L : LiveState) (hM : M.referencesClosed = true)
    (decode : RegistryDecoder) (owner : String) (encode : String → AgentId)
    (hencode : Function.Injective encode) (defaultAgent : AgentId) (runtimeAvailable : Finset AgentId) :
    (publish L M).toResolvedSnapshot decode owner (M.agentNames owner) encode hencode
      defaultAgent runtimeAvailable =
    snapshotFromDesired M.docs decode owner (M.agentNames owner) encode hencode
      defaultAgent runtimeAvailable := by
  unfold LiveState.toResolvedSnapshot
  rw [publish_realizes L M hM]

theorem publication_coverage
    (M : Manifest) (L : LiveState) (hM : M.referencesClosed = true)
    (decode : RegistryDecoder) (owner : String) (encode : String → AgentId)
    (hencode : Function.Injective encode) (defaultAgent : AgentId) (runtimeAvailable : Finset AgentId) :
    let snapshot := (publish L M).toResolvedSnapshot decode owner (M.agentNames owner)
      encode hencode defaultAgent runtimeAvailable
    snapshot.runnable ∪ snapshot.unavailable = (M.agentNames owner).image encode := by
  simp only [publication_resolved_snapshot M L hM]
  exact snapshot_coverage M.docs decode owner (M.agentNames owner) encode hencode
    defaultAgent runtimeAvailable

theorem publication_rejected_configuration_unavailable
    (M : Manifest) (L : LiveState) (hM : M.referencesClosed = true)
    (decode : RegistryDecoder) (owner name : String) (encode : String → AgentId)
    (hencode : Function.Injective encode) (hname : name ∈ M.agentNames owner)
    (defaultAgent : AgentId) (runtimeAvailable : Finset AgentId)
    (err : Configuration.ResolveError)
    (h : Configuration.resolveAgent (decode M.docs) owner name = .error err) :
    encode name ∈ ((publish L M).toResolvedSnapshot decode owner (M.agentNames owner)
      encode hencode defaultAgent runtimeAvailable).unavailable := by
  rw [publication_resolved_snapshot M L hM]
  apply Finset.mem_sdiff.mpr
  refine ⟨Finset.mem_image.mpr ⟨name, hname, rfl⟩, ?_⟩
  exact rejected_configuration_not_runnable M.docs decode owner name (M.agentNames owner)
    encode hencode defaultAgent runtimeAvailable err h

end ApplyReconcile
