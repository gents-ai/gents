import Mathlib.Data.Nat.Basic

namespace ToolPresentation

/-- Presentation is a projection of the selected surface, never a grant. Hidden
names absent from that surface are inert. -/
def advertised (granted hidden : Bool) : Bool := granted && !hidden

/-- Presentation admission does not own unknown provider names: those retain
on_tool_call and ordinary unknown-tool dispatch. Composition must hold a grant. -/
inductive AdmissionDecision where
  | allowed
  | hidden
  | unavailable
  deriving DecidableEq, Repr

def admissionDecision (granted hidden providerOrigin : Bool) : AdmissionDecision :=
  if providerOrigin then
    if granted && hidden then .hidden else .allowed
  else if granted then .allowed else .unavailable

def isGateway (name : String) : Bool :=
  ["script", "config", "load_skill", "spawn_process"].contains name

/-- Nonempty presentation configuration needs an already-selected, visible
script with tool access, and cannot hide a presentation gateway. Availability
is checked again when the runtime constructs the effective surface. -/
def configurationValid (hasHidden allSelected scriptReady gatewayHidden : Bool) : Bool :=
  !hasHidden || (allSelected && scriptReady && !gatewayHidden)

theorem ungranted_composition_refused (hidden : Bool) :
    admissionDecision false hidden false = .unavailable := by simp [admissionDecision]

theorem hidden_provider_call_refused :
    admissionDecision true true true = .hidden := by simp [admissionDecision]

theorem composition_unchanged (granted hidden : Bool) :
    admissionDecision granted hidden false = (if granted then .allowed else .unavailable) := rfl

theorem empty_presentation_unchanged (granted : Bool) :
    advertised granted false = granted := by simp [advertised]

theorem hidden_configuration_requires_script (allSelected gatewayHidden : Bool) :
    configurationValid true allSelected false gatewayHidden = false := by
  simp [configurationValid]

/-- Runtime readiness can only turn hiding off, never grant execution. -/
def effectiveHidden (hidden scriptReady : Bool) : Bool := hidden && scriptReady

theorem unknown_provider_uses_existing_owner (hidden : Bool) :
    admissionDecision false hidden true = .allowed := by simp [admissionDecision]

theorem empty_provider_uses_existing_owner (granted : Bool) :
    admissionDecision granted false true = .allowed := by simp [admissionDecision]

theorem unavailable_script_advertises_grant (granted hidden : Bool) :
    advertised granted (effectiveHidden hidden false) = granted := by
  simp [effectiveHidden, advertised]

end ToolPresentation
