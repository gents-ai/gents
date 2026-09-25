import Proofs.ConfigDefaults
import Proofs.ToolPolicy.Instances

/-! Target authored selection and timeout contracts. `Surface` remains a resolved
capability ceiling, not a second persisted Tools configuration. Discovery is an
observation input; neither it nor presentation can add selected names. -/

namespace ToolPolicy

inductive RemoteToolStyle where
  | flat
  | discovery
  deriving DecidableEq, Repr

structure RemoteServiceTools where
  toolNames : Finset String
  backgroundNames : Finset String
  style : RemoteToolStyle := .discovery
  required : Bool := false

abbrev RemoteSelection := String → Option RemoteServiceTools
/-- Registry enablement and discovery are distinct observations, both scoped to
principal and logical service name. Selection is already behavior-local. -/
abbrev RemoteAvailability := (String × String) → Finset String
abbrev RemoteEnablement := (String × String) → Bool

/-- Configuration validation rejects background names outside the whitelist. -/
def RemoteServiceTools.valid (s : RemoteServiceTools) : Bool :=
  decide (s.backgroundNames ⊆ s.toolNames)

/-- Admission rejects malformed per-service configuration before runtime grants
are projected. The empty grant fallback below is only a defensive invocation check. -/
def admitRemoteService (s : RemoteServiceTools) : Option RemoteServiceTools :=
  if s.valid then some s else none

theorem invalid_remote_selection_rejected (s : RemoteServiceTools)
    (h : ¬ s.backgroundNames ⊆ s.toolNames) : admitRemoteService s = none := by
  simp [admitRemoteService, RemoteServiceTools.valid, h]

/-- Optional outages do not block behavior admission; required services must
be enabled and observed available. Invocation always applies both gates. -/
def serviceReady (s : RemoteServiceTools) (enabled : Bool)
    (available : Option (Finset String)) : Bool :=
  s.valid && (!s.required || (enabled && available.isSome))

theorem required_unavailable_rejected (s : RemoteServiceTools) (enabled : Bool)
    (h : s.required = true) : serviceReady s enabled none = false := by
  simp [serviceReady, h]

theorem required_disabled_rejected (s : RemoteServiceTools)
    (available : Option (Finset String)) (h : s.required = true) :
    serviceReady s false available = false := by simp [serviceReady, h]

theorem optional_unavailable_admitted (s : RemoteServiceTools) (enabled : Bool)
    (hr : s.required = false) (hv : s.valid = true) :
    serviceReady s enabled none = true := by simp [serviceReady, hr, hv]

theorem required_available_admitted (s : RemoteServiceTools) (names : Finset String)
    (hr : s.required = true) (hv : s.valid = true) :
    serviceReady s true (some names) = true := by simp [serviceReady, hr, hv]

/-- Resolve selected names through the same principal-scoped registry gate
used by readiness. Cached discovery cannot enable a disabled service. -/
def remoteGrants (selected : RemoteSelection) (available : RemoteAvailability)
    (enabled : RemoteEnablement) (agent service : String) : Finset String :=
  match selected service with
  | none => ∅
  | some s => if s.valid && enabled (agent, service) then
      s.toolNames ∩ available (agent, service) else ∅

def remoteBackground (selected : RemoteSelection) (available : RemoteAvailability)
    (enabled : RemoteEnablement) (agent service : String) : Finset String :=
  match selected service with
  | none => ∅
  | some s => s.backgroundNames ∩ remoteGrants selected available enabled agent service

theorem omitted_remote_grants_none (available : RemoteAvailability)
    (enabled : RemoteEnablement) (agent service : String) :
    remoteGrants (fun _ => none) available enabled agent service = ∅ := rfl

theorem disabled_remote_grants_none (selected : RemoteSelection)
    (available : RemoteAvailability) (enabled : RemoteEnablement)
    (agent service : String) (h : enabled (agent, service) = false) :
    remoteGrants selected available enabled agent service = ∅ := by
  cases hs : selected service <;> simp [remoteGrants, hs, h]

/-- Grants depend only on the selected principal/service pair; registry or
health observations for other principals cannot change this invocation. -/
theorem remote_grants_owner_local (selected : RemoteSelection)
    (a b : RemoteAvailability) (e f : RemoteEnablement) (agent service : String)
    (ha : a (agent, service) = b (agent, service))
    (he : e (agent, service) = f (agent, service)) :
    remoteGrants selected a e agent service = remoteGrants selected b f agent service := by
  cases hs : selected service <;> simp [remoteGrants, hs, ha, he]

theorem remote_grants_selected (selected : RemoteSelection) (available : RemoteAvailability)
    (enabled : RemoteEnablement) (agent service name : String)
    (h : name ∈ remoteGrants selected available enabled agent service) :
    ∃ s, selected service = some s ∧ name ∈ s.toolNames := by
  unfold remoteGrants at h
  split at h
  · simp at h
  · next s hs =>
    split at h
    · exact ⟨s, hs, (Finset.mem_inter.mp h).1⟩
    · simp at h

theorem background_subset_remote (selected : RemoteSelection) (available : RemoteAvailability)
    (enabled : RemoteEnablement) (agent service : String) :
    remoteBackground selected available enabled agent service ⊆
      remoteGrants selected available enabled agent service := by
  unfold remoteBackground
  split
  · exact Finset.empty_subset _
  · exact Finset.inter_subset_right

/-- Changing presentation preserves invocation permissions. -/
theorem presentation_invariant (s : RemoteServiceTools) (style : RemoteToolStyle)
    (available : RemoteAvailability) (enabled : RemoteEnablement) (agent service : String) :
    remoteGrants (fun _ => some {s with style := style}) available enabled agent service =
      remoteGrants (fun _ => some s) available enabled agent service := rfl

/-- Exact name selection is enforced together with the existing resolved ceiling,
including calls through discovery wrappers. -/
def remoteExecutable (behavior ceiling runtime : Surface) (selected : RemoteSelection)
    (available : RemoteAvailability) (enabled : RemoteEnablement)
    (agent service name : String) : Prop :=
  (effective behavior ceiling runtime).mcpServices.permits service ∧
    name ∈ remoteGrants selected available enabled agent service

theorem remote_executable_within_ceiling (behavior ceiling runtime : Surface)
    (selected : RemoteSelection) (available : RemoteAvailability) (enabled : RemoteEnablement)
    (agent service name : String)
    (h : remoteExecutable behavior ceiling runtime selected available enabled agent service name) :
    ceiling.mcpServices.permits service ∧
      ∃ s, selected service = some s ∧ name ∈ s.toolNames := by
  exact ⟨effective_mcp_subset_ceiling behavior ceiling runtime service h.1,
    remote_grants_selected selected available enabled agent service name h.2⟩

/-! ## Capability timeouts use the shared signed-limit decoder -/

/-- Documented defaults (seconds): bash execution 120, wait 30, maxWait 600,
background 36000; remote execution 300, connect 15, discovery 30, stale 120,
background 36000, wait 30, maxWait 600. -/
def bashExecutionDefault : Nat := 120
def bashWaitDefault : Nat := 30
def bashMaxWaitDefault : Nat := 600
def bashBackgroundDefault : Nat := 36000
def remoteExecutionDefault : Nat := 300
def remoteConnectDefault : Nat := 15
def remoteDiscoveryDefault : Nat := 30
def remoteStaleDefault : Nat := 120
def remoteBackgroundDefault : Nat := 36000
def remoteWaitDefault : Nat := 30
def remoteMaxWaitDefault : Nat := 600

/-- Bash/remote timeout controls: `maxExecution` absent follows the resolved
execution default. -/
structure BashTimeouts where
  execution : Option Int
  maxExecution : Option Int
  wait : Option Int
  maxWait : Option Int
  background : Option Int

/-- Stale-health and background lifetime caps can only narrow the normal
service-call timeout; backgrounding does not bypass the call cap. -/
def capTimeout (normal : Nat) : Option Nat → Nat
  | none => normal
  | some extra => min normal extra

theorem timeout_cap_never_extends (normal : Nat) (extra : Option Nat) :
    capTimeout normal extra ≤ normal := by
  cases extra <;> simp [capTimeout]

theorem timeout_cap_obeys_extra (normal extra : Nat) :
    capTimeout normal (some extra) ≤ extra := Nat.min_le_right _ _

/-- Shared bounded-pair validation; defaults stay with their owning capability. -/
def resolvedRemoteWait (wait maximum : Option Int) : Option (Nat × Nat) :=
  ConfigDefaults.resolveBounded remoteWaitDefault (some remoteMaxWaitDefault) wait maximum

def resolvedLspTimeout (timeout maximum : Option Int) : Option (Nat × Nat) :=
  ConfigDefaults.resolveBounded 20 (some 300) timeout maximum

/-- The whole bash document rejects invalid limits; no partial tuple is admitted. -/
def resolvedBash (l : BashTimeouts) : Option (Nat × Nat × Nat × Nat × Nat) := do
  let execution ← ConfigDefaults.resolveBounded bashExecutionDefault none l.execution l.maxExecution
  let wait ← ConfigDefaults.resolveBounded bashWaitDefault (some bashMaxWaitDefault) l.wait l.maxWait
  let background ← ConfigDefaults.resolveNat bashBackgroundDefault 1 l.background
  pure (execution.1, execution.2, wait.1, wait.2, background)

theorem resolvedBash_maxima_cover_defaults (l : BashTimeouts)
    (v : Nat × Nat × Nat × Nat × Nat) (h : resolvedBash l = some v) :
    v.1 ≤ v.2.1 ∧ v.2.2.1 ≤ v.2.2.2.1 := by
  simp only [resolvedBash, bind, Option.bind] at h
  split at h <;> simp_all
  next execution he =>
    split at h <;> simp_all
    next wait hw =>
      split at h <;> simp_all
      cases h
      exact ⟨ConfigDefaults.resolved_maximum_covers_default _ _ _ _ _ he,
        ConfigDefaults.resolved_maximum_covers_default _ _ _ _ _ hw⟩

theorem resolvedBash_max_follows_exec (l : BashTimeouts)
    (v : Nat × Nat × Nat × Nat × Nat) (hm : l.maxExecution = none)
    (h : resolvedBash l = some v) : v.2.1 = v.1 := by
  simp only [resolvedBash, hm, bind, Option.bind] at h
  split at h <;> simp_all
  next execution he =>
    split at h <;> simp_all
    split at h <;> simp_all
    cases h
    exact ConfigDefaults.absent_maximum_follows _ _ _ he

/-- The same bound check covers all configured maximum/default pairs. -/
theorem other_capability_small_maxima_rejected :
    resolvedRemoteWait none (some 5) = none ∧
    resolvedLspTimeout none (some 5) = none := by decide

theorem bash_small_maximum_rejected :
    resolvedBash ⟨none, some 5, none, none, none⟩ = none := rfl

/-- The complete bash defaults pass the same admission path as authored values. -/
theorem bash_defaults_resolve :
    resolvedBash ⟨none, none, none, none, none⟩ = some (120, 120, 30, 600, 36000) := rfl

/-! ## Effective timeouts: admitted document values under host ceilings

A Tools document is validated on its own terms above; the host then narrows it.
Deployment ceilings differ per host (`--command-timeout-secs` and
`--command-timeout-max-secs`), so an admitted value above one is clamped rather
than rejected: the same document stays valid on every host. Fixed runtime
ceilings (background lifetime, observation wait, LSP action) clamp the same way.
An absent value keeps the host's own default, so unconfigured documents keep the
existing deployment behavior exactly. -/

/-- Host foreground command bounds. The host never caps below its default. -/
structure CommandCeiling where
  default : Nat
  maximum : Nat
  deriving Repr, DecidableEq

def CommandCeiling.cap (c : CommandCeiling) : Nat := max c.default c.maximum

/-- Fixed ceilings: background lifetime, observation wait, and LSP action. -/
def backgroundLifetimeCeiling : Nat := 36000
def waitCeiling : Nat := 600
def lspActionCeiling : Nat := 300
/-- A model-requested LSP action shorter than this cannot finish indexing retries. -/
def lspActionFloor : Nat := 5

/-- Effective bash foreground `(default, maximum)`. An authored default without
an authored maximum fixes both, matching `resolvedBash_max_follows_exec`; with
neither authored the host pair applies unchanged. -/
def effectiveBashForeground (c : CommandCeiling) (l : BashTimeouts) : Option (Nat × Nat) :=
  match resolvedBash l with
  | none => none
  | some _ =>
    let authoredDefault := l.execution.map Int.toNat
    let maximum := min ((l.maxExecution.map Int.toNat).getD (authoredDefault.getD c.cap)) c.cap
    some (min (authoredDefault.getD c.default) maximum, maximum)

theorem effectiveBashForeground_bounded (c : CommandCeiling) (l : BashTimeouts)
    (d m : Nat) (h : effectiveBashForeground c l = some (d, m)) : d ≤ m ∧ m ≤ c.cap := by
  unfold effectiveBashForeground at h
  split at h
  · cases h
  · simp only [Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    exact ⟨Nat.min_le_right _ _, Nat.min_le_right _ _⟩

theorem effectiveBashForeground_unauthored (c : CommandCeiling) :
    effectiveBashForeground c ⟨none, none, none, none, none⟩ = some (c.default, c.cap) := by
  unfold effectiveBashForeground
  rw [bash_defaults_resolve]
  simp [CommandCeiling.cap]

/-- An authored CLI timeout replaces the host registration's value, clamped to
the host foreground cap; unauthored keeps the registration unchanged. -/
def effectiveCliTimeout (c : CommandCeiling) (registration : Nat) (authored : Option Int) :
    Option Nat :=
  match authored with
  | none => some registration
  | some _ => (ConfigDefaults.resolveNat registration 1 authored).map (min · c.cap)

theorem effectiveCliTimeout_authored_within_cap (c : CommandCeiling) (registration : Nat)
    (value : Int) (t : Nat) (h : effectiveCliTimeout c registration (some value) = some t) :
    t ≤ c.cap := by
  simp only [effectiveCliTimeout, Option.map_eq_some'] at h
  obtain ⟨_, _, rfl⟩ := h
  exact Nat.min_le_right _ _

/-- Background lifetime for a spawned bash or remote call. The remote service
call cap still applies inside it (`capTimeout`). -/
def effectiveBackgroundLifetime (authored : Option Int) : Option Nat :=
  (ConfigDefaults.resolveNat bashBackgroundDefault 1 authored).map (min · backgroundLifetimeCeiling)

theorem effectiveBackgroundLifetime_within_ceiling (authored : Option Int) (t : Nat)
    (h : effectiveBackgroundLifetime authored = some t) : t ≤ backgroundLifetimeCeiling := by
  simp only [effectiveBackgroundLifetime, Option.map_eq_some'] at h
  obtain ⟨_, _, rfl⟩ := h
  exact Nat.min_le_right _ _

/-- Narrow an admitted `(default, maximum)` pair under a fixed ceiling. -/
def underCeiling (ceiling : Nat) (pair : Nat × Nat) : Nat × Nat :=
  let maximum := min pair.2 ceiling
  (min pair.1 maximum, maximum)

/-- Observation wait for one `wait_process` call. Waiting never cancels work. -/
def waitFor (policy : Nat × Nat) : Option Nat → Nat
  | none => policy.1
  | some requested => min (max requested 1) policy.2

/-- Action timeout for one LSP call; the floor applies only to model requests. -/
def lspActionFor (policy : Nat × Nat) : Option Nat → Nat
  | none => policy.1
  | some requested => min (max requested lspActionFloor) policy.2

theorem waitFor_within_ceiling (pair : Nat × Nat) (requested : Option Nat) :
    waitFor (underCeiling waitCeiling pair) requested ≤ waitCeiling := by
  cases requested <;> simp only [waitFor, underCeiling] <;> omega

theorem lspActionFor_within_ceiling (pair : Nat × Nat) (requested : Option Nat) :
    lspActionFor (underCeiling lspActionCeiling pair) requested ≤ lspActionCeiling := by
  cases requested <;> simp only [lspActionFor, underCeiling] <;> omega

/-- Unconfigured documents keep the previous fixed behavior exactly. -/
theorem unconfigured_wait_and_lsp_defaults :
    (resolvedRemoteWait none none).map (underCeiling waitCeiling) = some (30, 600) ∧
    (resolvedLspTimeout none none).map (underCeiling lspActionCeiling) = some (20, 300) ∧
    effectiveBackgroundLifetime none = some 36000 := by decide

end ToolPolicy
