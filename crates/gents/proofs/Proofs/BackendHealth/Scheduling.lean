import Proofs.BackendHealth.State

namespace Proofs.BackendHealth

/-- Revision represents only connection and discovery inputs. Catalogs, probe
observations, labels and capacity changes do not schedule additional attempts:
otherwise the prober's own writes could consume the failure threshold. -/
structure ProbeConfiguration where
  backendId : Nat
  revision : Nat
  enabled : Bool
  deriving DecidableEq, BEq, Repr

def shouldProbe (known : List ProbeConfiguration) (current : ProbeConfiguration)
    (periodic : Bool) : Bool :=
  current.enabled && (periodic || !known.contains current)

def scheduledProbes (known current : List ProbeConfiguration) (periodic : Bool) : List Nat :=
  (current.filter (shouldProbe known · periodic)).map (·.backendId)

/-- Disabled or removed configurations leave the observed set, so a later
observed enablement receives an initial probe again. Failed reads must not be
translated to an empty successful observation. -/
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

end Proofs.BackendHealth
