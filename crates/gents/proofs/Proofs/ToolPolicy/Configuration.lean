import Proofs.ConfigDefaults
import Proofs.ToolPolicy.Instances
import Proofs.ToolPolicy.PluginInvocation

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
abbrev RemoteAvailability := (String × String) → Finset String
abbrev RemoteEnablement := (String × String) → Bool

def RemoteServiceTools.valid (s : RemoteServiceTools) : Bool :=
  decide (s.backgroundNames ⊆ s.toolNames)

def admitRemoteService (s : RemoteServiceTools) : Option RemoteServiceTools :=
  if s.valid then some s else none

theorem invalid_remote_selection_rejected (s : RemoteServiceTools)
    (h : ¬ s.backgroundNames ⊆ s.toolNames) : admitRemoteService s = none := by
  simp [admitRemoteService, RemoteServiceTools.valid, h]

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

theorem presentation_invariant (s : RemoteServiceTools) (style : RemoteToolStyle)
    (available : RemoteAvailability) (enabled : RemoteEnablement) (agent service : String) :
    remoteGrants (fun _ => some {s with style := style}) available enabled agent service =
      remoteGrants (fun _ => some s) available enabled agent service := rfl

def remoteExecutable (agentSurface ceiling runtime : Surface) (selected : RemoteSelection)
    (available : RemoteAvailability) (enabled : RemoteEnablement)
    (agent service name : String) : Prop :=
  (effective agentSurface ceiling runtime).mcpServices.permits service ∧
    name ∈ remoteGrants selected available enabled agent service

theorem remote_executable_within_ceiling (agentSurface ceiling runtime : Surface)
    (selected : RemoteSelection) (available : RemoteAvailability) (enabled : RemoteEnablement)
    (agent service name : String)
    (h : remoteExecutable agentSurface ceiling runtime selected available enabled agent service name) :
    ceiling.mcpServices.permits service ∧
      ∃ s, selected service = some s ∧ name ∈ s.toolNames := by
  exact ⟨effective_mcp_subset_ceiling agentSurface ceiling runtime service h.1,
    remote_grants_selected selected available enabled agent service name h.2⟩

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

structure BashTimeouts where
  execution : Option Int
  maxExecution : Option Int
  wait : Option Int
  maxWait : Option Int
  background : Option Int

def capTimeout (normal : Nat) : Option Nat → Nat
  | none => normal
  | some extra => min normal extra

theorem timeout_cap_never_extends (normal : Nat) (extra : Option Nat) :
    capTimeout normal extra ≤ normal := by
  cases extra <;> simp [capTimeout]

theorem timeout_cap_obeys_extra (normal extra : Nat) :
    capTimeout normal (some extra) ≤ extra := Nat.min_le_right _ _

def resolvedRemoteWait (wait maximum : Option Int) : Option (Nat × Nat) :=
  ConfigDefaults.resolveBounded remoteWaitDefault (some remoteMaxWaitDefault) wait maximum

def resolvedLspTimeout (timeout maximum : Option Int) : Option (Nat × Nat) :=
  ConfigDefaults.resolveBounded 20 (some 300) timeout maximum

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

theorem other_capability_small_maxima_rejected :
    resolvedRemoteWait none (some 5) = none ∧
    resolvedLspTimeout none (some 5) = none := by decide

theorem bash_small_maximum_rejected :
    resolvedBash ⟨none, some 5, none, none, none⟩ = none := rfl

theorem bash_defaults_resolve :
    resolvedBash ⟨none, none, none, none, none⟩ = some (120, 120, 30, 600, 36000) := rfl

structure CommandCeiling where
  default : Nat
  maximum : Nat
  deriving Repr, DecidableEq

def CommandCeiling.cap (c : CommandCeiling) : Nat := max c.default c.maximum

def backgroundLifetimeCeiling : Nat := 36000
def waitCeiling : Nat := 600
def lspActionCeiling : Nat := 300
def lspActionFloor : Nat := 5

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

def effectiveBackgroundLifetime (authored : Option Int) : Option Nat :=
  (ConfigDefaults.resolveNat bashBackgroundDefault 1 authored).map (min · backgroundLifetimeCeiling)

theorem effectiveBackgroundLifetime_within_ceiling (authored : Option Int) (t : Nat)
    (h : effectiveBackgroundLifetime authored = some t) : t ≤ backgroundLifetimeCeiling := by
  simp only [effectiveBackgroundLifetime, Option.map_eq_some'] at h
  obtain ⟨_, _, rfl⟩ := h
  exact Nat.min_le_right _ _

def underCeiling (ceiling : Nat) (pair : Nat × Nat) : Nat × Nat :=
  let maximum := min pair.2 ceiling
  (min pair.1 maximum, maximum)

def waitFor (policy : Nat × Nat) : Option Nat → Nat
  | none => policy.1
  | some requested => min (max requested 1) policy.2

def lspActionFor (policy : Nat × Nat) : Option Nat → Nat
  | none => policy.1
  | some requested => min (max requested lspActionFloor) policy.2

theorem waitFor_within_ceiling (pair : Nat × Nat) (requested : Option Nat) :
    waitFor (underCeiling waitCeiling pair) requested ≤ waitCeiling := by
  cases requested <;> simp only [waitFor, underCeiling] <;> omega

theorem lspActionFor_within_ceiling (pair : Nat × Nat) (requested : Option Nat) :
    lspActionFor (underCeiling lspActionCeiling pair) requested ≤ lspActionCeiling := by
  cases requested <;> simp only [lspActionFor, underCeiling] <;> omega

theorem unconfigured_wait_and_lsp_defaults :
    (resolvedRemoteWait none none).map (underCeiling waitCeiling) = some (30, 600) ∧
    (resolvedLspTimeout none none).map (underCeiling lspActionCeiling) = some (20, 300) ∧
    effectiveBackgroundLifetime none = some 36000 := by decide

def pluginResourceConsented (requested previous : Nat) (consent : Bool) : Bool :=
  consent || decide (requested ≤ previous)

/-- A host ceiling bounds per-call resources even if an approved pack
declaration requests more. -/
def effectivePluginResource (baseline requested ceiling : Nat) : Nat :=
  min (max baseline requested) ceiling

theorem plugin_resource_no_unapproved_raise (requested previous : Nat)
    (h : pluginResourceConsented requested previous false = true) : requested ≤ previous := by
  simpa [pluginResourceConsented] using h

theorem plugin_resource_within_host (baseline requested ceiling : Nat) :
    effectivePluginResource baseline requested ceiling ≤ ceiling := by
  exact Nat.min_le_right _ _

def pluginModelSlotAllowed (declared optional agentFree : Bool) : Bool :=
  declared && optional && agentFree

theorem plugin_model_slot_optional (declared optional agentFree : Bool)
    (h : pluginModelSlotAllowed declared optional agentFree = true) : optional = true := by
  cases declared <;> cases optional <;> cases agentFree <;> simp_all [pluginModelSlotAllowed]

inductive PluginAccess where
  | read
  | readWrite
  deriving DecidableEq, Repr

namespace PluginAccess

def rank : PluginAccess → Nat
  | .read => 0
  | .readWrite => 1

def spelling : PluginAccess → String
  | .read => "read"
  | .readWrite => "read_write"

end PluginAccess

def pluginBindingValid (declared : PluginAccess) (hasWriteFields : Bool) : Bool :=
  declared == .readWrite || !hasWriteFields

def pluginCallAccess (declared : PluginAccess) (hasWriteFields setsWriteField : Bool) :
    PluginAccess :=
  match declared with
  | .read => .read
  | .readWrite => if hasWriteFields && !setsWriteField then .read else .readWrite

def pluginCallAdmitted (declared : PluginAccess) (granted : Option PluginAccess)
    (hasWriteFields setsWriteField : Bool) : Bool :=
  match granted with
  | none => false
  | some granted =>
    decide ((pluginCallAccess declared hasWriteFields setsWriteField).rank ≤ granted.rank)

theorem plugin_call_access_within_declared (declared : PluginAccess)
    (hasWriteFields setsWriteField : Bool) :
    (pluginCallAccess declared hasWriteFields setsWriteField).rank ≤ declared.rank := by
  cases declared <;> cases hasWriteFields <;> cases setsWriteField <;> decide

theorem plugin_call_admitted_within_grant (declared granted : PluginAccess)
    (hasWriteFields setsWriteField : Bool)
    (h : pluginCallAdmitted declared (some granted) hasWriteFields setsWriteField = true) :
    (pluginCallAccess declared hasWriteFields setsWriteField).rank ≤ granted.rank := by
  simpa [pluginCallAdmitted] using h

theorem plugin_call_outside_grant_refused (declared : PluginAccess)
    (hasWriteFields setsWriteField : Bool) :
    pluginCallAdmitted declared none hasWriteFields setsWriteField = false := rfl

theorem plugin_reading_call_needs_only_read (setsWriteField : Bool)
    (h : setsWriteField = false) :
    pluginCallAdmitted .readWrite (some .read) true setsWriteField = true := by
  subst h; decide

theorem plugin_writing_call_needs_read_write (hasWriteFields : Bool) :
    pluginCallAdmitted .readWrite (some .read) hasWriteFields true = false := by
  cases hasWriteFields <;> decide

/-- Who installed a pack: the operator, or an agent through self-configuration.
Defined here so job authority reuses it without an import cycle. -/
inductive InstallProvenance where
  | operator
  | selfConfig
  deriving DecidableEq, Repr

def scratchMaxBytes : Nat := 1048576
def scratchMaxFiles : Nat := 32

/-- The run directory is named by the calling row's `tool_call_id`, never by
its handle: the host suffixes duplicate handles and a resumed row inherits one. -/
def scratchRunDir (cwd toolCallId : String) : String :=
  cwd ++ "/.gents/workflows/runs/" ++ toolCallId ++ "/"

def scratchName (name : String) : Bool :=
  name != "" && name != "." && name != ".." && !name.contains '/'

/-- A scratch write is admitted at its run-directory path when the name and
the `tool_call_id` are each a single path component, `cwdAllowed` covers the path, the file is at most 1 MiB
and the row holds at most 32 scratch files including this one. -/
def scratchAdmitted (cwd : String) (cwdAllowed : String → Bool)
    (toolCallId name : String) (size count : Nat) : Option String :=
  let path := scratchRunDir cwd toolCallId ++ name
  if scratchName name && scratchName toolCallId && cwdAllowed path && decide (size ≤ scratchMaxBytes) &&
      decide (count ≤ scratchMaxFiles) then some path else none

theorem scratch_within_run_dir (cwd : String) (cwdAllowed : String → Bool)
    (toolCallId name : String) (size count : Nat) (path : String)
    (h : scratchAdmitted cwd cwdAllowed toolCallId name size count = some path) :
    path = scratchRunDir cwd toolCallId ++ name ∧ scratchName name = true ∧
      scratchName toolCallId = true ∧ cwdAllowed path = true := by
  dsimp only [scratchAdmitted] at h
  split at h
  · rename_i hc
    simp only [Bool.and_eq_true] at hc
    cases h
    exact ⟨rfl, hc.1.1.1.1, hc.1.1.1.2, hc.1.1.2⟩
  · cases h

theorem scratch_bounded (cwd : String) (cwdAllowed : String → Bool)
    (toolCallId name : String) (size count : Nat) (path : String)
    (h : scratchAdmitted cwd cwdAllowed toolCallId name size count = some path) :
    size ≤ scratchMaxBytes ∧ count ≤ scratchMaxFiles := by
  dsimp only [scratchAdmitted] at h
  split at h
  · rename_i hc
    simp only [Bool.and_eq_true, decide_eq_true_eq] at hc
    exact ⟨hc.1.2, hc.2⟩
  · cases h

/-- For a row's cwd the admitted path is a function of `tool_call_id` and
`name` only; no handle is an input, so handle suffixing and resume inheritance
leave it unchanged. -/
theorem scratch_dir_independent_of_handle (cwd : String) (cwdAllowed : String → Bool)
    (toolCallId name : String) (size count : Nat) (path : String)
    (h : scratchAdmitted cwd cwdAllowed toolCallId name size count = some path) :
    path = scratchRunDir cwd toolCallId ++ name :=
  (scratch_within_run_dir cwd cwdAllowed toolCallId name size count path h).1

end ToolPolicy
