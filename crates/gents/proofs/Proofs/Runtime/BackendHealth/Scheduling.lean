import Proofs.Runtime.BackendHealth.State

namespace Proofs.BackendHealth

/-- Revision represents only connection and discovery inputs: otherwise the
prober's own writes could consume the failure threshold. -/
structure ProbeConfiguration where
  backendId : Nat
  revision : Nat
  enabled : Bool
  deriving DecidableEq, BEq, Repr

/-- The backend fields an operator edits. -/
inductive BackendField where
  | provider
  | wireApi
  | endpoint
  | auth
  | connectTimeout
  | discoveryTimeout
  | name
  | tags
  | capacity
  | queueCapacity
  deriving DecidableEq, Repr

/-- Connection and discovery inputs advance the probe revision; labels and
admission limits do not, so editing them never triggers an off-cycle probe. -/
def BackendField.revisesProbe : BackendField → Bool
  | .provider | .wireApi | .endpoint | .auth | .connectTimeout | .discoveryTimeout => true
  | .name | .tags | .capacity | .queueCapacity => false

/-- One enabled backend at revision zero, after an edit of `field`. -/
def editedConfiguration (field : BackendField) : ProbeConfiguration :=
  { backendId := 0, revision := if field.revisesProbe then 1 else 0, enabled := true }

def shouldProbe (known : List ProbeConfiguration) (current : ProbeConfiguration)
    (periodic : Bool) : Bool :=
  current.enabled && (periodic || !known.contains current)

def scheduledProbes (known current : List ProbeConfiguration) (periodic : Bool) : List Nat :=
  (current.filter (shouldProbe known · periodic)).map (·.backendId)

/-- Disabled or removed configurations leave the observed set; failed reads must
not be translated to an empty successful observation. -/
def observedProbeConfigurations (current : List ProbeConfiguration) : List ProbeConfiguration :=
  current.filter (·.enabled)

theorem disabled_not_probed (known : List ProbeConfiguration) (current : ProbeConfiguration)
    (periodic : Bool) (h : current.enabled = false) :
    shouldProbe known current periodic = false := by
  simp [shouldProbe, h]

theorem new_enabled_probed (known : List ProbeConfiguration) (current : ProbeConfiguration)
    (enabled : current.enabled = true) (new : known.contains current = false) :
    shouldProbe known current false = true := by
  simp [shouldProbe, enabled, new]

theorem unchanged_waits_for_periodic (known : List ProbeConfiguration)
    (current : ProbeConfiguration) (present : known.contains current = true) :
    shouldProbe known current false = false := by
  simp [shouldProbe, present]

theorem periodic_probes_enabled (known : List ProbeConfiguration)
    (current : ProbeConfiguration) (enabled : current.enabled = true) :
    shouldProbe known current true = true := by
  simp [shouldProbe, enabled]

/-- Measured health outlives a cycle exactly while its backend is observed as
enabled. A cancelled cycle observes nothing and prunes nothing: shutdown must not
read as the disappearance of every backend. -/
def retainedEntries (entries observed : List Nat) (cancelled : Bool) : List Nat :=
  if cancelled then entries else entries.filter (observed.contains ·)

theorem cancelled_cycle_keeps_every_entry (entries observed : List Nat) :
    retainedEntries entries observed true = entries := by
  simp [retainedEntries]

theorem observed_entry_survives (entries observed : List Nat) (id : Nat)
    (present : id ∈ entries) (enabled : observed.contains id = true) :
    id ∈ retainedEntries entries observed false := by
  have hm : id ∈ observed := List.contains_iff_mem.mp enabled
  simp [retainedEntries, present, hm]

theorem unobserved_entry_is_pruned (entries observed : List Nat) (id : Nat)
    (absent : observed.contains id = false) :
    id ∉ retainedEntries entries observed false := by
  have hn : id ∉ observed := fun h => by
    have hc : observed.contains id = true := List.contains_iff_mem.mpr h
    simp_all
  simp [retainedEntries, hn]

end Proofs.BackendHealth
