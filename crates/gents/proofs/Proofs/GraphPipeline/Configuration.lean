import Proofs.Configuration
import Proofs.GraphPipeline
import Proofs.Triggers.Groups

namespace GraphPipeline

/-- Projection of an installed StageCapability: reference and caller selection.
Schemas/versioning remain in the existing compiler validation boundary. -/
structure CapabilitySelection where
  agentDid : String
  taskId : String
  allowedCallers : List String

/-- A graph resolves a capability's existing Task under that task's principal.
Foreign capabilities are usable when authorized; no foreign config is installed
or rewritten, and there is no graph-specific behavior/model override. -/
def resolveStage (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (caller capabilityId : String) :
    Except Configuration.ResolveError Configuration.ResolvedSessionConfig :=
  match capabilities capabilityId with
  | none => .error .missingCapability
  | some cap =>
    if caller ∈ cap.allowedCallers then
      Configuration.resolveTask registry cap.agentDid cap.taskId
    else .error .callerNotAllowed

theorem empty_callers_denied (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (caller capabilityId : String) (cap : CapabilitySelection)
    (hc : capabilities capabilityId = some cap) (he : cap.allowedCallers = []) :
    resolveStage capabilities registry caller capabilityId = .error .callerNotAllowed := by
  simp [resolveStage, hc, he]

theorem stage_uses_common_task_resolver (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (caller capabilityId : String) (cap : CapabilitySelection)
    (hc : capabilities capabilityId = some cap) (ha : caller ∈ cap.allowedCallers) :
    resolveStage capabilities registry caller capabilityId =
      Configuration.resolveTask registry cap.agentDid cap.taskId := by
  simp [resolveStage, hc, ha]

/-- Graph delivery applies its cardinality bounds to the same resolved candidate
used by ordinary event triggers, including counts loaded from source fields. -/
def resolveGroup (config : Triggers.Groups.GroupConfig) (correlationField : String)
    (mode : ConcurrencyMode) (key : Triggers.Groups.EventGroupKey)
    (fields : String → Option Int) (actual : Nat) (timedOut : Bool) :
    Option Triggers.Groups.Candidate :=
  if config.validForGraph correlationField mode then
    (Triggers.Groups.resolveCandidate config correlationField key fields actual timedOut).bind fun c =>
      match c.expectedCount with
      | none => none
      | some n => if 2 ≤ n then some c else none
  else none

theorem graph_delivery_count_bounded (config : Triggers.Groups.GroupConfig)
    (correlationField : String) (mode : ConcurrencyMode)
    (key : Triggers.Groups.EventGroupKey) (fields : String → Option Int)
    (actual : Nat) (timedOut : Bool) (candidate : Triggers.Groups.Candidate)
    (h : resolveGroup config correlationField mode key fields actual timedOut = some candidate) :
    ∃ n, candidate.expectedCount = some n ∧ 2 ≤ n ∧ n ≤ Triggers.Groups.maxGroupDocs := by
  unfold resolveGroup at h
  split at h
  · cases hc : Triggers.Groups.resolveCandidate config correlationField key fields actual timedOut with
    | none => simp [hc] at h
    | some c =>
      cases hn : c.expectedCount with
      | none => simp [hc, hn] at h
      | some n =>
        simp only [hc, Option.bind, hn] at h
        split at h
        · next hv =>
          have he := Option.some.inj h
          subst candidate
          exact ⟨n, hn, hv, Triggers.Groups.resolved_expected_count_bounded
            config correlationField key fields actual timedOut c n hc hn⟩
        · simp at h
  · simp at h

end GraphPipeline
