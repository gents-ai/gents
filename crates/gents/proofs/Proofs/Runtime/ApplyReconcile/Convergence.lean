import Proofs.Runtime.ApplyReconcile.RuntimeBridge
import Proofs.Documents.ApplyReconcile.Publication

namespace ApplyReconcile

/-- An admitted application realizes the merged desired documents before the
same canonical registry decoder and readiness projection run. -/
theorem application_resolved_snapshot
    (p : Plan) (L : LiveState) (hp : admits (p.over L.desired) p.changed = true)
    (decode : RegistryDecoder) (owner : String) (encode : String → AgentDid)
    (hencode : Function.Injective encode) (defaultAgent : AgentDid) (runtimeAvailable : Finset AgentDid) :
    (applyPlan L p).toResolvedSnapshot decode owner ((p.over L.desired).agentNames owner) encode
      hencode defaultAgent runtimeAvailable =
    snapshotFromDesired (p.over L.desired).docs decode owner ((p.over L.desired).agentNames owner)
      encode hencode defaultAgent runtimeAvailable := by
  unfold LiveState.toResolvedSnapshot
  rw [applyPlan_admitted L p hp]

theorem application_coverage
    (p : Plan) (L : LiveState) (hp : admits (p.over L.desired) p.changed = true)
    (decode : RegistryDecoder) (owner : String) (encode : String → AgentDid)
    (hencode : Function.Injective encode) (defaultAgent : AgentDid) (runtimeAvailable : Finset AgentDid) :
    let snapshot := (applyPlan L p).toResolvedSnapshot decode owner
      ((p.over L.desired).agentNames owner) encode hencode defaultAgent runtimeAvailable
    snapshot.runnable ∪ snapshot.unavailable =
      ((p.over L.desired).agentNames owner).image encode := by
  simp only [application_resolved_snapshot p L hp]
  exact snapshot_coverage (p.over L.desired).docs decode owner
    ((p.over L.desired).agentNames owner) encode hencode defaultAgent runtimeAvailable

theorem application_rejected_configuration_unavailable
    (p : Plan) (L : LiveState) (hp : admits (p.over L.desired) p.changed = true)
    (decode : RegistryDecoder) (owner name : String) (encode : String → AgentDid)
    (hencode : Function.Injective encode) (hname : name ∈ (p.over L.desired).agentNames owner)
    (defaultAgent : AgentDid) (runtimeAvailable : Finset AgentDid)
    (err : Configuration.ResolveError)
    (h : Configuration.resolveAgent (decode (p.over L.desired).docs) owner name = .error err) :
    encode name ∈ ((applyPlan L p).toResolvedSnapshot decode owner
      ((p.over L.desired).agentNames owner) encode hencode defaultAgent runtimeAvailable).unavailable := by
  rw [application_resolved_snapshot p L hp]
  apply Finset.mem_sdiff.mpr
  refine ⟨Finset.mem_image.mpr ⟨name, hname, rfl⟩, ?_⟩
  exact rejected_configuration_not_runnable (p.over L.desired).docs decode owner name
    ((p.over L.desired).agentNames owner) encode hencode defaultAgent runtimeAvailable err h

end ApplyReconcile
