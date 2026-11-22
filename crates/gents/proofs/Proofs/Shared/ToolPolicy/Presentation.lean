import Proofs.Shared.Basic

/-!
# Tool presentation: which granted tools the model is shown

A Tools document may list granted tools whose definitions are not sent to the
model (`hidden_tool_names`). A hidden tool stays on the request's surface: a
composing plugin (`script`) lists, describes and calls it as before. Hiding is
presentation, never authority: it grants nothing and withdraws nothing a
composing call could reach.
-/

namespace ToolPresentation

/-- Who asks for a call: the model, through the provider's response, or a
composing plugin through the effect dispatcher. -/
inductive Origin where
  | provider
  | composition
  deriving DecidableEq, Repr

inductive AdmissionDecision where
  | allowed
  | hidden
  | unavailable
  deriving DecidableEq, Repr

/-- The presentation gate before a call's hooks run. A hidden, granted tool
is refused to the model, which is told to call it through `script`. A name the
surface does not grant keeps its existing owners when the model asks (the
admission hook, then the unknown-tool failure), and is unavailable to a
composing plugin. -/
def admissionDecision (granted hidden : Bool) (origin : Origin) : AdmissionDecision :=
  match origin with
  | .provider => if granted && hidden then .hidden else .allowed
  | .composition => if granted then .allowed else .unavailable

/-- The tools a model needs to reach every other one: hiding them would leave
no way to call what is hidden or to repair the configuration. -/
def isGateway (name : String) : Bool :=
  ["script", "config", "load_skill", "spawn_process"].contains name

/-- A listed tool is hidden only while `script`, the way left to call it, is
on the built surface. When `script` is not selected, or a ceiling removed it,
every granted tool is shown and the runtime warns; a gateway is never hidden. -/
def effectiveHidden (listed gateway scriptPresent : Bool) : Bool :=
  listed && !gateway && scriptPresent

/-- A written Tools document hides no gateway, and hides another tool only
while it selects `script` with tool calls. -/
def configurationValid (hidesOthers scriptSelected hidesGateway : Bool) : Bool :=
  !hidesGateway && (!hidesOthers || scriptSelected)

theorem hidden_provider_call_refused :
    admissionDecision true true .provider = .hidden := rfl

theorem composition_ignores_hiding (granted hidden : Bool) :
    admissionDecision granted hidden .composition =
      admissionDecision granted false .composition := rfl

theorem ungranted_composition_refused (hidden : Bool) :
    admissionDecision false hidden .composition = .unavailable := rfl

theorem unknown_provider_name_keeps_its_owners (hidden : Bool) :
    admissionDecision false hidden .provider = .allowed := by
  cases hidden <;> rfl

/-- Hiding never admits a call the same surface without hiding would refuse. -/
theorem hiding_never_admits (granted : Bool) (origin : Origin)
    (h : admissionDecision granted true origin = .allowed) :
    admissionDecision granted false origin = .allowed := by
  cases granted <;> cases origin <;> simp_all [admissionDecision]

theorem nothing_hidden_without_script (listed gateway : Bool) :
    effectiveHidden listed gateway false = false := by
  simp [effectiveHidden]

theorem gateway_never_hidden (listed scriptPresent : Bool) :
    effectiveHidden listed true scriptPresent = false := by
  simp [effectiveHidden]

theorem hiding_requires_script (hidesGateway : Bool) :
    configurationValid true false hidesGateway = false := by
  cases hidesGateway <;> rfl

end ToolPresentation
