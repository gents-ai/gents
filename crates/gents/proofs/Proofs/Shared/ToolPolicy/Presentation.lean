import Mathlib.Data.Nat.Basic

namespace ToolPresentation

/-- Presentation is a projection of the selected surface, never a grant. Hidden
names absent from that surface are inert. -/
def advertised (granted hidden : Bool) : Bool := granted && !hidden

/-- Provider-origin calls use the advertised surface. Composing invocations
retain the captured granted surface and their existing admission owners. -/
def directAdmissible (granted hidden : Bool) : Bool := advertised granted hidden

def admissible (granted hidden providerOrigin : Bool) : Bool :=
  if providerOrigin then directAdmissible granted hidden else granted

def isGateway (name : String) : Bool :=
  ["script", "config", "load_skill", "spawn_process"].contains name

/-- Nonempty presentation configuration needs an already-selected, visible
script with tool access, and cannot hide a presentation gateway. Availability
is checked again when the runtime constructs the effective surface. -/
def configurationValid (hasHidden allSelected scriptReady gatewayHidden : Bool) : Bool :=
  !hasHidden || (allSelected && scriptReady && !gatewayHidden)

theorem hiding_never_grants (hidden providerOrigin : Bool) :
    admissible false hidden providerOrigin = false := by
  cases providerOrigin <;> simp [admissible, directAdmissible, advertised]

theorem hidden_provider_call_refused (granted : Bool) :
    admissible granted true true = false := by
  simp [admissible, directAdmissible, advertised]

theorem composition_unchanged (granted hidden : Bool) :
    admissible granted hidden false = granted := rfl

theorem empty_presentation_unchanged (granted : Bool) :
    advertised granted false = granted := by simp [advertised]

theorem hidden_configuration_requires_script (allSelected gatewayHidden : Bool) :
    configurationValid true allSelected false gatewayHidden = false := by
  simp [configurationValid]

/-- Presentation refusal applies only to a selected hidden provider name.
Unknown provider names retain the loop's existing hook/unknown-tool owner. -/
def presentationRefused (granted hidden providerOrigin : Bool) : Bool :=
  granted && hidden && providerOrigin

/-- Runtime readiness can only turn hiding off, never grant execution. -/
def effectiveHidden (hidden scriptReady : Bool) : Bool := hidden && scriptReady

theorem unknown_provider_uses_existing_owner (hidden : Bool) :
    presentationRefused false hidden true = false := by simp [presentationRefused]

theorem empty_provider_uses_existing_owner (granted : Bool) :
    presentationRefused granted false true = false := by simp [presentationRefused]

theorem unavailable_script_advertises_grant (granted hidden : Bool) :
    advertised granted (effectiveHidden hidden false) = granted := by
  simp [effectiveHidden, advertised]

end ToolPresentation
