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
  deriving DecidableEq, Repr

/-- What `init` leaves for each core pack, every one of which it installs.
Dynamic workflows is installed but not enabled (maintainer decision
2026-11-13): it starts agents from scripts the model writes, so it waits for
the user to turn it on. Script runs the Engineer's own tools from a script,
which is what it is for, so it is wired with `toolCalls`. -/
def initWiring : CorePack → Wiring
  | .dynamicWorkflows => ⟨false, false, false, false⟩
  | .script => ⟨true, true, false, true⟩
  | _ => ⟨true, false, false, true⟩

/-- What the user's explicit enable operation leaves: dynamic workflows wired
with its agents' reader and the dynamic consent; any other core pack as init
leaves it. -/
def enabledWiring : CorePack → Wiring
  | .dynamicWorkflows => ⟨true, false, true, true⟩
  | pack => initWiring pack

theorem init_never_consents_dynamic (pack : CorePack) :
    (initWiring pack).consentDynamic = false := by
  cases pack <;> rfl

theorem dynamic_workflows_present_but_unwired :
    (initWiring .dynamicWorkflows).wired = false ∧
    (initWiring .dynamicWorkflows).rootReaders = false := ⟨rfl, rfl⟩

theorem every_other_core_pack_is_wired (pack : CorePack) (h : pack ≠ .dynamicWorkflows) :
    (initWiring pack).wired = true := by
  cases pack <;> simp_all [initWiring]

theorem only_script_calls_tools (pack : CorePack) :
    (initWiring pack).toolCalls = true ↔ pack = .script := by
  cases pack <;> simp [initWiring]

theorem enabling_consents_dynamic :
    (enabledWiring .dynamicWorkflows).consentDynamic = true := rfl

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
