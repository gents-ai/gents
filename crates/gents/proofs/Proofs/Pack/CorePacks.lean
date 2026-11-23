/-!
# Core packs

The Engineer's packs are core packs: their sources live in the runtime's
repository, they are built and tested with it, and a release carries exactly
the builds it was tested with (maintainer decision 2026-11-13,
gents-ai/gents#3744). They are still plugins in packs; the runtime adds only
the seam they need. `init` installs them from the release itself, with no
registry and no network.

A pack's plugins are built against the plugin interface: the host imports,
the call ABI and the manifest fields the runtime reads. The interface's version
is the Gents release (maintainer decision 2026-11-13): a pack manifest
declares the release it was built for, and a runtime loads a pack built for
its own major version and a release not newer than itself. The registry
remains the source of other packs and of newer builds of core packs, and its
archives pass the same check.
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

/-- Whether `init` refuses before reserving a home's bootstrap. A fresh
Engineer taking the default packs needs every archive its release carries:
without them it would come up with none of its skills while its prompt names
them. An existing home, a non-Engineer home and an explicit opt-out
(`--no-default-packs`) never need them. -/
def refuseInit (fresh engineer defaults allArchives : Bool) : Bool :=
  fresh && engineer && defaults && !allArchives

theorem fresh_engineer_missing_archive_refused : refuseInit true true true false = true := rfl

theorem existing_home_not_refused (engineer defaults allArchives : Bool) :
    refuseInit false engineer defaults allArchives = false := by
  simp [refuseInit]

theorem opt_out_not_refused (fresh engineer allArchives : Bool) :
    refuseInit fresh engineer false allArchives = false := by
  simp [refuseInit]

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

/-- What the install reads of the profile a pack's inference slot is bound to:
the profile with an enabled backend, with a disabled one, or with none. -/
inductive ProfileState
  | serving
  | backendDisabled
  | backendMissing
  deriving DecidableEq, Repr

inductive SlotBinding
  | bound
  /-- Bound to a profile that cannot serve yet; the pack's agents on it resolve
  to `Configuration.ResolveError.disabledBackend` and serve no request until
  its backend is enabled (`SelfConfig.deferred_agent_serves_no_request`). -/
  | deferred
  | refused
  deriving DecidableEq, Repr

/-- Whether an install binds a slot to the profile named for it.

Decision (maintainer, 2026-11-13, desktop first run on #3670): installing
Gents implies its core packs, and a node is set up before any inference
provider exists, its backend disabled. So `init` names the Engineer's own
profile for every required slot of a core pack, and a named profile whose
backend is disabled is bound and deferred, as the Engineer itself is
(`SelfConfig.candidateResolution`): the setup's inference step enables that
backend in place. A profile with no backend is refused. A slot nobody names is
still bound automatically only to a serving profile. -/
def slotBinding : ProfileState → SlotBinding
  | .serving => .bound
  | .backendDisabled => .deferred
  | .backendMissing => .refused

def SlotBinding.installs : SlotBinding → Bool
  | .refused => false
  | _ => true

theorem init_binds_before_inference : (slotBinding .backendDisabled).installs = true := rfl

theorem missing_backend_refused : slotBinding .backendMissing = .refused := rfl

theorem installs_iff (state : ProfileState) :
    (slotBinding state).installs = true ↔ state ≠ .backendMissing := by
  cases state <;> simp [slotBinding, SlotBinding.installs]
/-- Where the enable operation writes, given whether a runtime serves the
home and whether the home has the pack installed.

A runtime holds the home's store while it serves. Enabling a pack the home
has is a reader grant, a plugin wiring and an agent edit, each of which a
client of the running runtime makes through it (the wiring signed by the
client, `Configuration.wirePlanSubject`), so a running node is
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

/-- A Gents release version. Pre-release builds are release candidates,
`MAJOR.MINOR.PATCH-rcN`; `rc` is `none` for the release itself. -/
structure Release where
  major : Nat
  minor : Nat
  patch : Nat
  rc : Option Nat
  deriving DecidableEq, Repr

/-- Candidates of one triple in numeric order, all before the release. -/
def preLe : Option Nat → Option Nat → Bool
  | _, none => true
  | none, some _ => false
  | some a, some b => decide (a ≤ b)

/-- Release precedence: by major, minor and patch, then `preLe`. -/
def Release.le (a b : Release) : Bool :=
  decide (a.major < b.major) || (a.major == b.major &&
    (decide (a.minor < b.minor) || (a.minor == b.minor &&
      (decide (a.patch < b.patch) || (a.patch == b.patch && preLe a.rc b.rc)))))

theorem Release.le_refl (r : Release) : r.le r = true := by
  cases r with
  | mk major minor patch rc => cases rc <;> simp [Release.le, preLe]

/-- The release a manifest without a declaration counts as built for: 1.0.0-rc2,
the latest release before manifests carried the field, whose plugin interface
the runtime still implements unchanged. -/
def baseline : Release := ⟨1, 0, 0, some 2⟩

def builtFor (declared : Option Release) : Release := declared.getD baseline

inductive InterfaceAdmission
  | compatible
  /-- Built for an earlier major version: after a breaking release the pack
  needs a build for this one. -/
  | olderMajor
  /-- Built for a release newer than this runtime, or a later major version:
  a newer runtime, or an older build of the pack, is the next step. -/
  | tooNew
  deriving DecidableEq, Repr

/-- Whether `runtime` loads a pack built for `declared`: the same major
version and a release not newer than the runtime. Pre-releases follow release
precedence, so a candidate runtime loads packs built for its own or an
earlier candidate of the same major, and the release loads its candidates'
packs. Every archive is checked when it is opened, whatever its origin. -/
def interfaceAdmission (runtime : Release) (declared : Option Release) : InterfaceAdmission :=
  let built := builtFor declared
  if built.major < runtime.major then .olderMajor
  else if runtime.major < built.major then .tooNew
  else if built.le runtime then .compatible
  else .tooNew

theorem compatible_has_the_runtime_major (runtime : Release) (declared : Option Release)
    (h : interfaceAdmission runtime declared = .compatible) :
    (builtFor declared).major = runtime.major ∧ (builtFor declared).le runtime = true := by
  unfold interfaceAdmission at h
  by_cases h1 : (builtFor declared).major < runtime.major
  · simp [h1] at h
  · by_cases h2 : runtime.major < (builtFor declared).major
    · simp [h1, h2] at h
    · by_cases h3 : (builtFor declared).le runtime = true
      · exact ⟨by omega, h3⟩
      · simp [h1, h2, h3] at h

theorem own_release_is_compatible (runtime : Release) :
    interfaceAdmission runtime (some runtime) = .compatible := by
  simp [interfaceAdmission, builtFor, Release.le_refl]

theorem undeclared_loads_on_the_baseline :
    interfaceAdmission baseline none = .compatible := by decide

/-- What an update from the registry does with the archive it resolved. It
replaces the installed pack only when this runtime loads it; otherwise the
installed version stays and the refusal names the release. -/
inductive UpdateDecision | take | keepInstalled
  deriving DecidableEq, Repr

def updateDecision (candidate : InterfaceAdmission) : UpdateDecision :=
  if candidate == .compatible then .take else .keepInstalled

theorem incompatible_update_keeps_installed (candidate : InterfaceAdmission)
    (h : candidate ≠ .compatible) : updateDecision candidate = .keepInstalled := by
  cases candidate <;> simp_all [updateDecision]

end Pack.CorePacks
