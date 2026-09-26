import Proofs.Configuration
import Proofs.GraphPipeline
import Proofs.Triggers.Groups

namespace GraphPipeline

/-- What executes a stage: an existing Task (a model request), or an installed
plugin pinned to one artifact digest (no model request). -/
inductive StageTarget where
  | task (taskId : String)
  | plugin (plugin digest : String)
  deriving DecidableEq, Repr

/-- Projection of an installed StageCapability: reference and caller selection.
Schemas/versioning remain in the existing compiler validation boundary. -/
structure CapabilitySelection where
  agentDid : String
  target : StageTarget
  allowedCallers : List String

/-- The artifact digest a principal's host has installed under a plugin name. -/
abbrev PluginInstalls := String → String → Option String

/-- How a resolved stage runs. -/
inductive StageExecutor where
  | model (config : Configuration.ResolvedSessionConfig)
  | plugin (plugin digest : String)

/-- A plugin stage runs only the exact artifact it was admitted with. -/
def resolvePlugin (installs : PluginInstalls) (agentDid plugin digest : String) :
    Except Configuration.ResolveError StageExecutor :=
  match installs agentDid plugin with
  | none => .error .missingPlugin
  | some installed =>
    if installed = digest then .ok (.plugin plugin digest) else .error .pluginDigestMismatch

/-- A graph resolves a capability's target under that capability's principal.
Foreign capabilities are usable when authorized; no foreign config is installed
or rewritten, and there is no graph-specific behavior/model override. -/
def resolveStage (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (installs : PluginInstalls)
    (caller capabilityId : String) :
    Except Configuration.ResolveError StageExecutor :=
  match capabilities capabilityId with
  | none => .error .missingCapability
  | some cap =>
    if caller ∈ cap.allowedCallers then
      match cap.target with
      | .task taskId => (Configuration.resolveTask registry cap.agentDid taskId).map .model
      | .plugin plugin digest => resolvePlugin installs cap.agentDid plugin digest
    else .error .callerNotAllowed

theorem empty_callers_denied (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (installs : PluginInstalls)
    (caller capabilityId : String) (cap : CapabilitySelection)
    (hc : capabilities capabilityId = some cap) (he : cap.allowedCallers = []) :
    resolveStage capabilities registry installs caller capabilityId = .error .callerNotAllowed := by
  simp [resolveStage, hc, he]

theorem stage_uses_common_task_resolver (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (installs : PluginInstalls)
    (caller capabilityId : String) (cap : CapabilitySelection) (taskId : String)
    (hc : capabilities capabilityId = some cap) (ha : caller ∈ cap.allowedCallers)
    (ht : cap.target = .task taskId) :
    resolveStage capabilities registry installs caller capabilityId =
      (Configuration.resolveTask registry cap.agentDid taskId).map .model := by
  simp [resolveStage, hc, ha, ht]

/-- A plugin stage never resolves to a model request. -/
theorem plugin_stage_is_not_a_model_request (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (installs : PluginInstalls)
    (caller capabilityId : String) (cap : CapabilitySelection) (plugin digest : String)
    (hc : capabilities capabilityId = some cap) (ht : cap.target = .plugin plugin digest)
    (config : Configuration.ResolvedSessionConfig) :
    resolveStage capabilities registry installs caller capabilityId ≠ .ok (.model config) := by
  unfold resolveStage resolvePlugin
  simp only [hc, ht]
  split
  · split
    · simp
    · split <;> simp
  · simp

/-- A resolved plugin stage runs exactly the pinned artifact, and only when the
owner's host has that artifact installed: substituting bytes is refused. -/
theorem plugin_stage_runs_the_pinned_artifact (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (installs : PluginInstalls)
    (caller capabilityId : String) (cap : CapabilitySelection) (plugin digest : String)
    (hc : capabilities capabilityId = some cap) (ht : cap.target = .plugin plugin digest)
    (p d : String)
    (h : resolveStage capabilities registry installs caller capabilityId = .ok (.plugin p d)) :
    p = plugin ∧ d = digest ∧ installs cap.agentDid plugin = some digest ∧
      caller ∈ cap.allowedCallers := by
  unfold resolveStage resolvePlugin at h
  simp only [hc, ht] at h
  split at h
  · rename_i ha
    split at h
    · simp at h
    · rename_i installed hi
      split at h
      · rename_i heq
        simp at h
        obtain ⟨rfl, rfl⟩ := h
        exact ⟨rfl, rfl, heq ▸ hi, ha⟩
      · simp at h
  · simp at h

theorem plugin_digest_substitution_denied (capabilities : String → Option CapabilitySelection)
    (registry : Configuration.Registry) (installs : PluginInstalls)
    (caller capabilityId : String) (cap : CapabilitySelection) (plugin digest installed : String)
    (hc : capabilities capabilityId = some cap) (ha : caller ∈ cap.allowedCallers)
    (ht : cap.target = .plugin plugin digest)
    (hi : installs cap.agentDid plugin = some installed) (hne : installed ≠ digest) :
    resolveStage capabilities registry installs caller capabilityId =
      .error .pluginDigestMismatch := by
  simp [resolveStage, resolvePlugin, hc, ha, ht, hi, hne]

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
