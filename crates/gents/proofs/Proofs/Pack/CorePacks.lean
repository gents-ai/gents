/-!
# Core packs

The Engineer's packs are core packs: their sources live in the runtime's
repository, they are built and tested with it, and a release carries exactly
the builds it was tested with (maintainer decision 2026-11-13,
gents-ai/gents#3744). They are still plugins in packs; the runtime adds only
the seam they need. `init` installs them from the release itself, with no
registry and no network.

A pack's plugins are built against the plugin interface: the host imports,
the call ABI and the manifest fields the runtime reads. The interface carries
a version, a pack manifest declares the version it was built for, and a
runtime loads only the versions it implements. The registry remains the
source of other packs and of newer builds of core packs, and its archives
pass the same check.
-/

namespace Pack.CorePacks

/-- The packs a release carries for the Engineer. -/
inductive CorePack | packBuilder | web | dynamicWorkflows | charts | dataTables | script
  deriving DecidableEq, Repr

def CorePack.coordinate : CorePack → String
  | .packBuilder => "gents/pack_builder"
  | .web => "gents/web"
  | .dynamicWorkflows => "gents/dynamic_workflows"
  | .charts => "gents/charts"
  | .dataTables => "gents/data_tables"
  | .script => "gents/script"

def all : List CorePack := [.packBuilder, .web, .dynamicWorkflows, .charts, .dataTables, .script]

/-- Where the archive an install takes comes from. -/
inductive Origin
  /-- The runtime's own release: built from its repository and tested with it. -/
  | release
  /-- Anything else: the registry, a home's store, a file or a directory. -/
  | other
  deriving DecidableEq, Repr

/-- Whether an install waits for the operator to approve what it grants: the
permissions its plugins request, a reader on the Engineer's tool root, or a
consent. Installing Gents is the consent to the core packs its release
carries (maintainer decision 2026-11-13), so a release archive installs
without asking; any other archive that grants something asks. -/
def approvalRequired (origin : Origin) (grants : Bool) : Bool :=
  grants && origin == .other

theorem release_installs_without_approval (grants : Bool) :
    approvalRequired .release grants = false := by
  simp [approvalRequired]

theorem other_grants_ask : approvalRequired .other true = true := rfl

/-- What one core pack leaves on the Engineer. -/
structure Wiring where
  /-- Its plugins are selected in the Engineer's Tools and its skills are
  attached to the Engineer's Context. -/
  wired : Bool
  /-- Its plugins may call the Engineer's own tools. -/
  toolCalls : Bool
  /-- The node owner's consent to model-written dynamic scripts, which raises
  the shipped `check` block to `run` (`PluginJob.consentedDynamic`, applied by
  `Pack.PluginWiring.consentDynamic`). -/
  consentDynamic : Bool
  /-- The agents it ships read the Engineer's tool root. -/
  rootReaders : Bool
  /-- The agents it ships are enabled: the runtime reconciles them as
  runnable, builds their tool surfaces and counts them in the node's
  readiness. A disabled agent is none of these (`Runtime.StartupDegraded`). -/
  agentsActive : Bool
  deriving DecidableEq, Repr

/-- What `init` leaves for each core pack, every one of which it installs.
Dynamic workflows is installed but not enabled (maintainer decision
2026-11-13): it starts agents from scripts the model writes, so it waits for
the user to turn it on. Script runs the Engineer's own tools from a script,
which is what it is for, so it is wired with `toolCalls`.

Installed but not enabled means the pack's documents are on the node and
nothing of it runs: its plugins are not selected, its agents hold no reader on
the Engineer's tool root and are disabled. An enabled agent whose host tools
read a root it holds no reader on has no tool surface, which is a failed
admission the node reports as degraded on every reconcile (fresh-node smoke
of #3670, 2026-11-13); a disabled one is off and lacks nothing. -/
def initWiring : CorePack → Wiring
  | .dynamicWorkflows => ⟨false, false, false, false, false⟩
  | .script => ⟨true, true, false, true, true⟩
  | _ => ⟨true, false, false, true, true⟩

/-- What the user's explicit enable operation leaves: dynamic workflows wired
with the dynamic consent, its agents granted their reader and enabled
together; any other core pack as init leaves it. -/
def enabledWiring : CorePack → Wiring
  | .dynamicWorkflows => ⟨true, false, true, true, true⟩
  | pack => initWiring pack

theorem init_never_consents_dynamic (pack : CorePack) :
    (initWiring pack).consentDynamic = false := by
  cases pack <;> rfl

theorem dynamic_workflows_present_but_unwired :
    (initWiring .dynamicWorkflows).wired = false ∧
    (initWiring .dynamicWorkflows).rootReaders = false ∧
    (initWiring .dynamicWorkflows).agentsActive = false := ⟨rfl, rfl, rfl⟩

/-- An agent a core pack leaves enabled holds its reader, at init and after
enabling: no core pack's agent is ever enabled without the root its host
tools read. -/
theorem active_agents_hold_their_root (pack : CorePack) :
    ((initWiring pack).agentsActive = true → (initWiring pack).rootReaders = true) ∧
    ((enabledWiring pack).agentsActive = true → (enabledWiring pack).rootReaders = true) := by
  cases pack <;> simp [initWiring, enabledWiring]

/-- Enabling grants a pack's agents their reader and enables them in the same
operation: in what it leaves, the two are one. -/
theorem enabling_grants_and_activates_together (pack : CorePack) :
    (enabledWiring pack).agentsActive = (enabledWiring pack).rootReaders := by
  cases pack <;> rfl

theorem every_other_core_pack_is_wired (pack : CorePack) (h : pack ≠ .dynamicWorkflows) :
    (initWiring pack).wired = true := by
  cases pack <;> simp_all [initWiring]

theorem only_script_calls_tools (pack : CorePack) :
    (initWiring pack).toolCalls = true ↔ pack = .script := by
  cases pack <;> simp [initWiring]

theorem enabling_consents_dynamic :
    (enabledWiring .dynamicWorkflows).consentDynamic = true := rfl

/-- Where the enable operation writes, given whether a runtime serves the
home and whether the home has the pack installed.

A runtime holds the home's store while it serves. Enabling a pack the home
has is a reader grant, a plugin wiring and an agent edit, each of which a
client of the running runtime makes through it (the wiring through the
operator route, `Configuration.operatorWireAvailable`), so a running node is
enabled in place and picks the change up on its next reconcile. A pack the
home lacks is first installed as the Engineer, under the Engineer's key, from
the home's own store; only a stopped node gives that store, so that one
enable waits for the node to stop and names how to stop each way it runs. -/
inductive EnableRoute
  /-- No runtime serves the home: written through its own store. -/
  | home
  /-- Written through the runtime serving the home. -/
  | runtime
  /-- Refused before anything is written: stop the node, then enable. -/
  | stopFirst
  deriving DecidableEq, Repr

def enableRoute (served installed : Bool) : EnableRoute :=
  if !served then .home else if installed then .runtime else .stopFirst

/-- The enable `init` names for dynamic workflows, which init installed,
needs no stop. -/
theorem enabling_an_installed_pack_needs_no_stop (served : Bool) :
    enableRoute served true ≠ .stopFirst := by
  cases served <;> simp [enableRoute]

/-- The plugin interface this runtime implements, and the oldest version it
still loads. Raise `current` when a host import, the call ABI or a manifest
field the runtime reads changes incompatibly; raise `oldest` when the runtime
stops serving an older version. -/
def currentInterface : Nat := 1
def oldestInterface : Nat := 1

/-- A manifest that declares no interface was built before the interface had
a version, which is interface 1. -/
def builtFor (declared : Option Nat) : Nat := declared.getD 1

inductive InterfaceAdmission
  | compatible
  /-- Built for a version this runtime no longer serves: a newer build of the
  pack is the next step. -/
  | tooOld
  /-- Built for a version newer than this runtime: a newer runtime, or an
  older build of the pack, is the next step. -/
  | tooNew
  deriving DecidableEq, Repr

/-- Whether this runtime loads a pack built for `declared`. Every archive is
checked when it is opened, whatever its origin. -/
def interfaceAdmission (declared : Option Nat) : InterfaceAdmission :=
  let version := builtFor declared
  if version < oldestInterface then .tooOld
  else if currentInterface < version then .tooNew
  else .compatible

theorem undeclared_is_compatible : interfaceAdmission none = .compatible := rfl

theorem current_is_compatible : interfaceAdmission (some currentInterface) = .compatible := rfl

theorem compatible_within_range (declared : Option Nat)
    (h : interfaceAdmission declared = .compatible) :
    oldestInterface ≤ builtFor declared ∧ builtFor declared ≤ currentInterface := by
  unfold interfaceAdmission at h
  by_cases low : builtFor declared < oldestInterface
  · simp [low] at h
  · by_cases high : currentInterface < builtFor declared
    · simp [low, high] at h
    · omega

/-- What an update from the registry does with the archive it resolved. It
replaces the installed pack only when this runtime loads it; otherwise the
installed version stays and the refusal names the interface. -/
inductive UpdateDecision | take | keepInstalled
  deriving DecidableEq, Repr

def updateDecision (candidate : InterfaceAdmission) : UpdateDecision :=
  if candidate == .compatible then .take else .keepInstalled

theorem incompatible_update_keeps_installed (candidate : InterfaceAdmission)
    (h : candidate ≠ .compatible) : updateDecision candidate = .keepInstalled := by
  cases candidate <;> simp_all [updateDecision]

end Pack.CorePacks
