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

/-- The tools hidden by default once `script` can reach them.

Decision (owner): `charts`, `data_tables` and `browser_exec` carry the largest
definitions the Engineer is granted, sent with every request, and few requests
use them; the model reaches them through `script`, which lists, describes and
calls them. -/
def defaultHidden (name : String) : Bool :=
  ["charts", "data_tables", "browser_exec"].contains name

/-- Whether a wiring that changes a Tools document leaves one tool listed in
`hidden_tool_names`. `selected` is whether the document selects the tool and
`script` whether it selects `script` with tool calls, before and after.

A default-hidden tool is listed when the wiring makes it hideable for the first
time: it is selected while `script` is, or `script` is selected while it is.
This holds on every path that wires a pack into an agent: the setup that
creates the Engineer, and a pack wired after the document exists. A tool that
was already hideable keeps what the document says, so a tool an operator chose
to show stays shown and a re-wire or an upgrade changes nothing. A wiring that
leaves `script` unselected clears the list: hiding needs `script`
(`configurationValid`), and without it every tool is shown
(`effectiveHidden`). -/
def listedAfterWiring
    (defaultHidden listed selectedBefore scriptBefore selectedAfter scriptAfter : Bool) : Bool :=
  scriptAfter && (listed || (defaultHidden && selectedAfter && !(selectedBefore && scriptBefore)))

theorem defaults_are_not_gateways :
    (["charts", "data_tables", "browser_exec"].all fun name => defaultHidden name && !isGateway name)
      = true := by decide

/-- A wiring never leaves a listed tool without `script`. -/
theorem wiring_listing_needs_script (default listed selectedBefore scriptBefore selectedAfter : Bool) :
    listedAfterWiring default listed selectedBefore scriptBefore selectedAfter false = false := by
  simp [listedAfterWiring]

/-- A tool already hideable keeps the operator's choice, whichever it was. -/
theorem wiring_keeps_the_operator_choice (default listed selectedAfter : Bool) :
    listedAfterWiring default listed true true selectedAfter true = listed := by
  cases default <;> cases listed <;> cases selectedAfter <;> rfl

/-- Only a default-hidden tool is ever added to the list. -/
theorem wiring_adds_only_defaults
    (listed selectedBefore scriptBefore selectedAfter scriptAfter : Bool) :
    listedAfterWiring false listed selectedBefore scriptBefore selectedAfter scriptAfter =
      (scriptAfter && listed) := by
  simp [listedAfterWiring]

theorem default_hidden_when_script_arrives :
    listedAfterWiring true false true false true true = true := rfl

theorem default_hidden_when_it_arrives_beside_script :
    listedAfterWiring true false false true true true = true := rfl

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
