import Proofs.ToolCalls.PluginJob.Admission
import Proofs.Store.Identity.AgentTargetAccess

namespace PluginJob.TargetGrant

open ToolPolicy
open Identity.AgentTargetAccess (Selection startable)

/-- The caller ceiling the job driver builds in `step_context_for`
(`caller_targets`): every AgentTarget document the selection names in
`targetIds`, enabled or not, resolved to its endpoint. -/
def callerTargets (bindings : List TargetResolution.Binding) (s : Selection) :
    EndpointScope Target Unit :=
  TargetResolution.scope bindings s.targetIds

/-- The job ceiling the job driver builds in `step_context_for`
(`job_targets`): the AgentTarget documents the selection names in `targetIds`,
enabled or not, kept when the calling plugin's `job.agent_targets` (`ids`) names
them, resolved to their endpoints. -/
def jobTargets (bindings : List TargetResolution.Binding) (s : Selection) (ids : List String) :
    EndpointScope Target Unit :=
  TargetResolution.scope bindings (s.targetIds.filter ids.contains)

/-- Every target a plugin job can spawn into is in `startable`, whether or not
the AgentTools group is enabled. `ids` is one plugin's `job.agent_targets`, a
subset of the union over plugins in `jobTargetIds`. -/
theorem job_spawn_target_is_startable (ctx : StepContext) (pin : Pin)
    (bindings : List TargetResolution.Binding) (s : Selection) (ids : List String)
    (named : ids ⊆ s.jobTargetIds)
    (caller : ctx.callerTargets = callerTargets bindings s)
    (job : ctx.jobTargets = jobTargets bindings s ids) (t : Target)
    (spawnable : (effectiveTargets ctx pin).permits t) :
    (TargetResolution.scope bindings (startable s)).permits t := by
  obtain ⟨-, inJob, inCaller, -⟩ := (effectiveTargets_permits_iff ctx pin t).mp spawnable
  rw [job] at inJob
  rw [caller] at inCaller
  unfold jobTargets at inJob
  unfold callerTargets at inCaller
  unfold startable
  cases hs : s.enabled
  · rw [if_neg (by decide)]
    apply TargetResolution.scope_mono bindings _ _ _ t inJob
    intro wire member
    simp only [List.mem_filter, List.contains_iff_mem] at member ⊢
    exact ⟨member.1, named member.2⟩
  · rw [if_pos rfl]
    exact inCaller

/-- With the group disabled, the grant equals the job ceiling over every
plugin's job targets (`jobTargetIds`), met with the caller ceiling. -/
theorem disabled_grant_is_the_job_ceiling (bindings : List TargetResolution.Binding)
    (s : Selection) (off : s.enabled = false) (t : Target) :
    (TargetResolution.scope bindings (startable s)).permits t ↔
      ((jobTargets bindings s s.jobTargetIds).meet unitVM (callerTargets bindings s)).permits t := by
  rw [scope_meet_permits_iff]
  unfold startable jobTargets callerTargets
  rw [off, if_neg (by decide)]
  exact ⟨fun inJob => ⟨inJob, TargetResolution.scope_mono bindings _ _
    (fun _ member => (List.mem_filter.mp member).1) t inJob⟩, And.left⟩

end PluginJob.TargetGrant
