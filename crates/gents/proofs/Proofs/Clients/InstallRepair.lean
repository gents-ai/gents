import Lean
import Proofs.Runtime.RuntimeReconcile.UpdateHandoff

namespace InstallRepair

/-- `olderStore` is a store an earlier, incompatible Gents made (its policies, schema, lineage
marker or encryption differ), decided read-only before anything opens it for writing. This release
is fresh-init only by the maintainer's decision (migrations begin after 1.0), so such a home is
reported and left exactly as it was. `insecureKey` is a key file other users can read; the user fixes its mode.
`unfinishedSetup` is a home whose setup stopped before it installed every core pack. -/
inductive HomeState where
  | absent | healthy | interrupted | unreadableSettings | missingSettings
  | missingStoreKey | olderStore | newerStore | insecureKey | corruptStore | unfinishedSetup
  deriving Repr, DecidableEq, Lean.ToJson

inductive Version where
  | unknown | older | same | newer
  deriving Repr, DecidableEq, Lean.ToJson

inductive Step where
  | drain | backup | stop | coldBackup | updateService | archiveHome | start | health | rollback
  deriving Repr, DecidableEq, Lean.ToJson

/-- `ask` is a question to the user at a terminal; `unattended` is the same home with nobody to
ask, which stops with what is wrong and the call that fixes it. -/
inductive Blocker where
  | none | newerVersion | olderStore | insecureKey | ask | unattended | unfinishedSetup
  | busy | keychain | foreignProcess | unreadableHome
  deriving Repr, DecidableEq, Lean.ToJson

/-- What the user answered at a terminal about a home that cannot be used as it is. Only an answer
moves a home aside; continuing an unfinished setup installs the core packs it did not reach. -/
inductive Answer where
  | none | setAside | continueSetup
  deriving Repr, DecidableEq, Lean.ToJson

/-- What the user asked for. `run` is any command against a home, `gents --home X` among them;
`repoint` is `gents runtime service repoint`, the one operation that moves the per-user service to
another home. -/
inductive Operation where
  | run | repoint
  deriving Repr, DecidableEq, Lean.ToJson

structure Observation where
  home : HomeState
  version : Version
  /-- A terminal the question can be asked on. -/
  interactive : Bool
  answer : Answer
  busy : Bool
  keychainLocked : Bool
  foreignProcess : Bool
  readable : Bool
  servicePresent : Bool
  serviceRunning : Bool
  serviceProgramPresent : Bool
  serviceSameHome : Bool
  managedUpdate : Bool
  drainSupported : Bool
  /-- The home the service definition names, when it is not the chosen home, still exists. -/
  servedHomePresent : Bool
  operation : Operation
  deriving Repr, DecidableEq, Lean.ToJson

structure Plan where
  steps : List Step
  blocker : Blocker
  needsSetup : Bool
  deriving Repr, DecidableEq, Lean.ToJson

/-- A home that cannot be used as it is and that setup can replace once it is moved aside. An older
store is not among them: it is reported and kept as it is, never moved. -/
def needsArchive : HomeState → Bool
  | .interrupted | .unreadableSettings | .missingSettings | .missingStoreKey | .corruptStore => true
  | _ => false

/-- The answer that counts is one given at a terminal: nothing else stands for the user's choice. -/
def answered (o : Observation) : Answer := if o.interactive then o.answer else .none

/-- The home is moved aside only when the user asked for it. -/
def setsAside (o : Observation) : Bool :=
  (needsArchive o.home || o.home == .unfinishedSetup) && answered o == .setAside

def continuesSetup (o : Observation) : Bool :=
  o.home == .unfinishedSetup && answered o == .continueSetup

/-- An unfinished setup still holds a working node, which is used while nobody chose otherwise. -/
def usable (o : Observation) (archive : Bool) : Bool :=
  o.home == .healthy || (o.home == .unfinishedSetup && !archive)

def opt (c : Bool) (s : Step) : List Step := if c then [s] else []

theorem mem_opt (x s : Step) (c : Bool) : x ∈ opt c s ↔ c = true ∧ x = s := by
  cases c <;> simp [opt]

/-- The per-user service serves one home. When it serves another home that still exists, that
home's node is someone's working node (the smoke test of #3670 took a live node offline by
repointing it from `gents --home X`), so only the operation the user asks for by name moves it.
A service whose home is gone is repaired as before. -/
def keepsServedHome (o : Observation) : Bool :=
  o.servicePresent && !o.serviceSameHome && o.servedHomePresent && o.operation != .repoint

/-- Continuing a setup installs packs through the home's own store, so its node stops first, as it
does before the home is moved aside. -/
def serviceSteps (o : Observation) (archive setup : Bool) : List Step :=
  let update := !o.managedUpdate &&
    (o.version == .older || !o.serviceProgramPresent || !o.serviceSameHome)
  let stop := o.serviceRunning && (setup || update)
  let drain := stop && usable o archive
  let restart := update && o.serviceRunning && !setup
  opt (drain && !o.drainSupported) .backup ++ opt (drain && o.drainSupported) .drain ++
    opt stop .stop ++
    opt (update && usable o archive) .coldBackup ++
    opt archive .archiveHome ++
    opt update .updateService ++
    opt restart .start ++ opt restart .health

/-- What stops a repair before it changes anything. A home that cannot be used as it is waits for
the user's answer: a question at a terminal, else a refusal that names the call. An unfinished
setup with nobody to ask is used as it is; the caller says what it lacks. -/
def blockerOf (o : Observation) : Option Blocker :=
  if o.version == .newer || o.home == .newerStore then some .newerVersion
  else if o.home == .olderStore then some .olderStore
  else if o.home == .insecureKey then some .insecureKey
  else if !o.readable then some .unreadableHome
  else if o.keychainLocked then some .keychain
  else if o.foreignProcess then some .foreignProcess
  else if needsArchive o.home && !setsAside o then
    some (if o.interactive then .ask else .unattended)
  else if o.home == .unfinishedSetup && answered o == .none && o.interactive then
    some .unfinishedSetup
  else none

def runsSetup (o : Observation) : Bool :=
  setsAside o || continuesSetup o || o.home == .absent

def planSteps (o : Observation) : List Step :=
  if o.servicePresent && !keepsServedHome o then serviceSteps o (setsAside o) (runsSetup o)
  else if setsAside o then [.archiveHome] else []

/-- A process holding the home (`busy`) blocks only a plan that would change the home or the
service; a node the user started by hand for this home is used as it is. -/
def plan (o : Observation) : Plan :=
  match blockerOf o with
  | some blocker => ⟨[], blocker, false⟩
  | none =>
    if o.busy && (runsSetup o || !(planSteps o).isEmpty) then ⟨[], .busy, false⟩
    else ⟨planSteps o, .none, runsSetup o⟩

theorem service_steps_set_aside_only_when_told (o : Observation) (archive setup : Bool)
    (h : Step.archiveHome ∈ serviceSteps o archive setup) : archive = true := by
  simp [serviceSteps, mem_opt] at h
  exact h

theorem plan_steps_are_planned (o : Observation) (x : Step) (h : x ∈ (plan o).steps) :
    x ∈ planSteps o := by
  unfold plan at h
  split at h
  · simp at h
  · split at h <;> simp_all

theorem blocked_plan_changes_nothing (o : Observation) (blocker : Blocker)
    (h : blockerOf o = some blocker) : plan o = ⟨[], blocker, false⟩ := by
  simp [plan, h]

/-- No plan moves a home aside unless the user, at a terminal, asked for it. -/
theorem set_aside_only_when_asked (o : Observation) (h : Step.archiveHome ∈ (plan o).steps) :
    setsAside o = true := by
  have h := plan_steps_are_planned o _ h
  unfold planSteps at h
  split at h
  · exact service_steps_set_aside_only_when_told _ _ _ h
  · split at h
    · assumption
    · simp at h

theorem unattended_plan_never_sets_aside (o : Observation) (h : o.interactive = false) :
    Step.archiveHome ∉ (plan o).steps := by
  intro mem
  have := set_aside_only_when_asked o mem
  simp [setsAside, answered, h] at this

theorem older_store_is_blocked (o : Observation) (h : o.home = .olderStore) :
    ∃ blocker, blockerOf o = some blocker := by
  unfold blockerOf
  split <;> simp_all

/-- A home an older, incompatible Gents made is left exactly as it is: no step changes it or the
service, and no setup runs over it. -/
theorem older_store_is_left_as_it_is (o : Observation) (h : o.home = .olderStore) :
    (plan o).steps = [] ∧ (plan o).needsSetup = false := by
  obtain ⟨blocker, hb⟩ := older_store_is_blocked o h
  simp [blocked_plan_changes_nothing o blocker hb]

theorem insecure_key_is_blocked (o : Observation) (h : o.home = .insecureKey) :
    ∃ blocker, blockerOf o = some blocker := by
  unfold blockerOf
  split <;> simp_all

theorem insecure_key_is_left_as_it_is (o : Observation) (h : o.home = .insecureKey) :
    (plan o).steps = [] ∧ (plan o).needsSetup = false := by
  obtain ⟨blocker, hb⟩ := insecure_key_is_blocked o h
  simp [blocked_plan_changes_nothing o blocker hb]

/-- The steps that change what the per-user service runs or which home it serves. -/
def changesService (p : Plan) : Bool :=
  p.steps.any fun s => s == .drain || s == .stop || s == .updateService || s == .start

theorem plan_keeps_served_home (o : Observation) (h : keepsServedHome o = true) :
    changesService (plan o) = false := by
  unfold changesService
  rw [List.any_eq_false]
  intro x hx
  have hs := plan_steps_are_planned o x hx
  simp only [planSteps, h, Bool.not_true, Bool.and_false, Bool.false_eq_true, ite_false] at hs
  split at hs <;> simp_all

theorem only_repoint_moves_a_present_served_home (o : Observation)
    (present : o.servicePresent = true) (other : o.serviceSameHome = false)
    (served : o.servedHomePresent = true) (asked : o.operation = .run) :
    changesService (plan o) = false :=
  plan_keeps_served_home o (by simp [keepsServedHome, present, other, served, asked])

structure Inventory where
  keys : List Nat
  nodeIdentities : List Nat
  documents : List Nat
  archivedKeys : List Nat
  archivedIdentities : List Nat
  archivedDocuments : List Nat
  deriving Repr, DecidableEq

def applyStep (s : Inventory) : Step → Inventory
  | .archiveHome =>
    { keys := []
      nodeIdentities := []
      documents := []
      archivedKeys := s.archivedKeys ++ s.keys
      archivedIdentities := s.archivedIdentities ++ s.nodeIdentities
      archivedDocuments := s.archivedDocuments ++ s.documents }
  | _ => s

def keys (s : Inventory) : List Nat := s.archivedKeys ++ s.keys
def identities (s : Inventory) : List Nat := s.archivedIdentities ++ s.nodeIdentities
def documents (s : Inventory) : List Nat := s.archivedDocuments ++ s.documents

theorem step_never_deletes_keys (s : Inventory) (a : Step) :
    keys (applyStep s a) = keys s := by cases a <;> simp [applyStep, keys]

theorem step_never_deletes_node_identity (s : Inventory) (a : Step) :
    identities (applyStep s a) = identities s := by cases a <;> simp [applyStep, identities]

theorem step_never_deletes_documents (s : Inventory) (a : Step) :
    documents (applyStep s a) = documents s := by cases a <;> simp [applyStep, documents]

def applyPlan (s : Inventory) (steps : List Step) : Inventory := steps.foldl applyStep s

theorem repair_never_deletes_keys (steps : List Step) (s : Inventory) :
    keys (applyPlan s steps) = keys s := by
  induction steps generalizing s with
  | nil => rfl
  | cons a rest ih =>
    simp only [applyPlan, List.foldl_cons]
    exact (ih (applyStep s a)).trans (step_never_deletes_keys s a)

theorem repair_never_deletes_node_identity (steps : List Step) (s : Inventory) :
    identities (applyPlan s steps) = identities s := by
  induction steps generalizing s with
  | nil => rfl
  | cons a rest ih =>
    simp only [applyPlan, List.foldl_cons]
    exact (ih (applyStep s a)).trans (step_never_deletes_node_identity s a)

theorem repair_never_deletes_documents (steps : List Step) (s : Inventory) :
    documents (applyPlan s steps) = documents s := by
  induction steps generalizing s with
  | nil => rfl
  | cons a rest ih =>
    simp only [applyPlan, List.foldl_cons]
    exact (ih (applyStep s a)).trans (step_never_deletes_documents s a)

def healthy (running : Bool) : Observation :=
  ⟨.healthy, .same, false, .none, false, false, false, true, true, running, true, true, false,
   true, false, .run⟩

theorem healthy_repair_is_noop (running : Bool) :
    plan (healthy running) = ⟨[], .none, false⟩ := by
  cases running <;> decide

theorem older_running_backup_precedes_stop :
    plan { (healthy true) with version := .older, drainSupported := false } =
      ⟨[.backup, .stop, .coldBackup, .updateService, .start, .health], .none, false⟩ := by decide

theorem modern_running_drains_before_cold_backup :
    plan { (healthy true) with version := .older } =
      ⟨[.drain, .stop, .coldBackup, .updateService, .start, .health], .none, false⟩ := by decide

theorem repoint_moves_a_running_served_home :
    plan { healthy true with
      serviceSameHome := false
      servedHomePresent := true
      operation := .repoint } =
      ⟨[.drain, .stop, .coldBackup, .updateService, .start, .health], .none, false⟩ := by decide

theorem explicit_home_leaves_a_running_served_home :
    plan { healthy true with
      serviceSameHome := false
      servedHomePresent := true
      interactive := true } = ⟨[], .none, false⟩ := by decide

theorem hand_started_node_is_used_as_it_is :
    plan { (healthy false) with busy := true } = ⟨[], .none, false⟩ := by decide

/-- The onboarding test of #3670 chmodded a key, removed `init.json` or the store key, and a
non-interactive `gents -p` moved the whole home aside. Now it stops and says why. -/
theorem unattended_broken_home_is_refused :
    plan { healthy true with home := .missingSettings } = ⟨[], .unattended, false⟩ := by decide

theorem a_broken_home_is_asked_about_at_a_terminal :
    plan { healthy true with home := .missingStoreKey, interactive := true } =
      ⟨[], .ask, false⟩ := by decide

theorem an_answered_broken_home_is_set_aside :
    plan { healthy true with home := .missingStoreKey, interactive := true, answer := .setAside } =
      ⟨[.stop, .archiveHome], .none, true⟩ := by decide

theorem an_unfinished_setup_is_offered_to_continue :
    plan { healthy true with home := .unfinishedSetup, interactive := true } =
      ⟨[], .unfinishedSetup, false⟩ := by decide

theorem continuing_a_setup_stops_its_node_first :
    plan { healthy true with
      home := .unfinishedSetup, interactive := true, answer := .continueSetup } =
      ⟨[.drain, .stop], .none, true⟩ := by decide

theorem an_unattended_unfinished_setup_is_used_as_it_is :
    plan { healthy true with home := .unfinishedSetup } = ⟨[], .none, false⟩ := by decide

theorem an_rc2_home_upgrade_changes_nothing :
    plan { healthy true with
      home := .olderStore
      version := .older
      interactive := true
      answer := .setAside } = ⟨[], .olderStore, false⟩ := by decide

theorem newer_store_is_untouched (o : Observation) (h : o.home = .newerStore) :
    plan o = ⟨[], .newerVersion, false⟩ := by simp [plan, blockerOf, h]

theorem newer_node_is_untouched (o : Observation) (h : o.version = .newer) :
    plan o = ⟨[], .newerVersion, false⟩ := by simp [plan, blockerOf, h]

end InstallRepair

namespace InstallRepair

/-- What a read-only look at the home's store and keys found, before anything opens the store for
writing: the backend's own markers, the lineage marker, the store-encryption record, the key file
modes, and the store's collection policies and schema checked on a copy. `fine` covers no store yet
and a store this version opens. -/
inductive StoreVerdict where
  | fine | older | newer | corrupt | insecureKey | missingKey
  deriving Repr, DecidableEq, Lean.ToJson

structure HomeObservation where
  present : Bool
  ownedEntries : Bool
  bootstrapConsumed : Bool
  pending : Bool
  settingsPresent : Bool
  settingsReadable : Bool
  dataPresent : Bool
  keysPresent : Bool
  missingKeyRefusal : Bool
  /-- Setup recorded that it had core packs left to install when it stopped. -/
  setupUnfinished : Bool
  store : StoreVerdict
  deriving Repr, DecidableEq, Lean.ToJson

/-- A store made by another version is named before anything about the files around it, so a home
an older Gents made is reported as that and never moved aside as broken. -/
def classifyHome (o : HomeObservation) : HomeState :=
  if !o.present || (!o.ownedEntries && !o.bootstrapConsumed) then .absent
  else if o.store == .newer then .newerStore
  else if o.store == .older then .olderStore
  else if o.store == .insecureKey then .insecureKey
  else if o.pending || (o.bootstrapConsumed && !o.settingsPresent) then .interrupted
  else if !o.settingsReadable then .unreadableSettings
  else if !o.settingsPresent then
    if !o.dataPresent && !o.keysPresent then .absent else .missingSettings
  else if o.missingKeyRefusal || o.store == .missingKey then .missingStoreKey
  else if o.store == .corrupt then .corruptStore
  else if o.setupUnfinished then .unfinishedSetup
  else .healthy

theorem an_older_store_is_never_classified_as_broken (o : HomeObservation)
    (present : o.present = true) (owned : o.ownedEntries = true) (h : o.store = .older) :
    classifyHome o = .olderStore := by
  simp [classifyHome, present, owned, h]

inductive ServedNode where
  | none | same | different
  deriving Repr, DecidableEq, Lean.ToJson

structure ServiceObservation where
  definitionPresent : Bool
  sameHome : Bool
  beforeSetup : Bool
  homeComplete : Bool
  executablePresent : Bool
  logCurrent : Bool
  loaded : Bool
  running : Bool
  failed : Bool
  wasServing : Bool
  version : Version
  foreignProgram : Bool
  managedUpdate : Bool
  serving : ServedNode
  /-- The home the definition names, when it is not the chosen home, still exists. -/
  servedHomePresent : Bool
  operation : Operation
  deriving Repr, DecidableEq, Lean.ToJson

/-- `rewriteDefinition` writes back a deleted definition while its job still runs this home's
node, which would otherwise be gone at the next login. A job the service manager still holds
after its definition was deleted, and that is not this home's node, is stopped: the service manager
keeps restarting it, and each restart would take the store of a home that setup is creating
(#3737). Whether a job is loaded is asked of the service manager, never read from the file. -/
inductive ServiceAction where
  | move | stop | reinstall | switch | refreshLog | restart | rewriteDefinition
  deriving Repr, DecidableEq, Lean.ToJson

structure ServiceStep where
  action : ServiceAction
  startAfter : Bool := false
  replaceProgram : Bool := false
  answered : Bool := false
  deriving Repr, DecidableEq, Lean.ToJson

def servicePlan (s : ServiceObservation) : List ServiceStep := Id.run do
  if !s.definitionPresent then
    return if !s.loaded then []
      else if s.serving == .same then [⟨.rewriteDefinition, false, false, false⟩]
      else [⟨.stop, false, false, false⟩]
  if !s.sameHome then
    if s.servedHomePresent && s.operation != .repoint then return []
    if !s.beforeSetup && !s.homeComplete then return []
    return [⟨.move, (s.loaded || s.wasServing) && s.homeComplete, s.foreignProgram, false⟩]
  if !s.executablePresent then return [⟨.reinstall, s.loaded && s.homeComplete, false, false⟩]
  let answered := s.version == .older && s.running && !s.managedUpdate
  let silent := s.version == .unknown && s.foreignProgram && !s.running
  if answered || silent then
    return [⟨.switch, (s.loaded || s.failed) && s.homeComplete, false, answered⟩]
  let stop := (s.loaded && !s.homeComplete) || (s.running && (s.serving == .different))
  return (if stop then [⟨.stop, false, false, false⟩] else []) ++
    (if !s.logCurrent then
      [⟨.refreshLog, s.loaded && s.homeComplete && !(s.serving == .same) && !stop, false, false⟩]
     else []) ++
    (if s.failed && !s.loaded && s.homeComplete && !(s.serving == .same) then
      [⟨.restart, false, false, false⟩] else [])

theorem a_deleted_definition_of_a_running_node_is_written_back (s : ServiceObservation)
    (absent : s.definitionPresent = false) (loaded : s.loaded = true) (same : s.serving = .same) :
    servicePlan s = [⟨.rewriteDefinition, false, false, false⟩] := by
  simp [servicePlan, Id.run, absent, loaded, same]

theorem a_loaded_job_without_a_definition_is_stopped_before_setup (s : ServiceObservation)
    (absent : s.definitionPresent = false) (loaded : s.loaded = true)
    (other : s.serving ≠ .same) : servicePlan s = [⟨.stop, false, false, false⟩] := by
  simp [servicePlan, Id.run, absent, loaded, other]

theorem service_plan_keeps_served_home (s : ServiceObservation)
    (present : s.definitionPresent = true) (other : s.sameHome = false)
    (served : s.servedHomePresent = true) (asked : s.operation = .run) :
    servicePlan s = [] := by
  simp [servicePlan, Id.run, present, other, served, asked]

/-- The service definition: the home it serves and the program it runs. -/
structure Definition where
  home : Nat
  program : Nat
  deriving Repr, DecidableEq

def applyAction (target : Definition) (d : Definition) : ServiceAction → Definition
  | .move => { d with home := target.home, program := target.program }
  | .reinstall | .switch => { d with program := target.program }
  | .stop | .refreshLog | .restart | .rewriteDefinition => d

/-- A repair in progress: the definition now, and the backup taken before each step. -/
structure Execution where
  current : Definition
  backups : List Definition
  deriving Repr, DecidableEq

def executeStep (target : Definition) (e : Execution) (s : ServiceStep) : Execution :=
  ⟨applyAction target e.current s.action, e.backups ++ [e.current]⟩

def execute (target found : Definition) (steps : List ServiceStep) : Execution :=
  steps.foldl (executeStep target) ⟨found, []⟩

/-- Rolling back restores the backups newest first, which leaves the first one. -/
def rollBack (e : Execution) : Definition :=
  match e.backups with
  | [] => e.current
  | first :: _ => first

theorem execute_keeps_first_backup (target first : Definition) (steps : List ServiceStep)
    (e : Execution) (h : e.backups.head? = some first) :
    (steps.foldl (executeStep target) e).backups.head? = some first := by
  induction steps generalizing e with
  | nil => simpa using h
  | cons s rest ih =>
    apply ih
    cases hb : e.backups with
    | nil => simp [hb] at h
    | cons b bs => simp_all [executeStep]

/-- A command that changed the service definition and then fails, after any number of its steps,
leaves the definition as it found it. -/
theorem failed_repair_restores_definition (target found : Definition)
    (steps : List ServiceStep) (k : Nat) :
    rollBack (execute target found (steps.take k)) = found := by
  unfold execute
  cases h : steps.take k with
  | nil => rfl
  | cons s rest =>
    have := execute_keeps_first_backup target found rest
      (executeStep target ⟨found, []⟩ s) (by simp [executeStep])
    unfold rollBack
    simp only [List.foldl_cons]
    split <;> simp_all

structure RefusalObservation where
  fresh : Bool
  missingKey : Bool
  olderStore : Bool
  newerStore : Bool
  deriving Repr, DecidableEq, Lean.ToJson

/-- What a start does with a node's fresh refusal of its store. There is no action that moves the
home: a missing key is a question for the user, and every other refusal stops with its report. -/
inductive RefusalAction where
  | ignore | ask | block
  deriving Repr, DecidableEq, Lean.ToJson

def refusalAction (o : RefusalObservation) : RefusalAction :=
  if !o.fresh then .ignore
  else if o.newerStore || o.olderStore then .block
  else if o.missingKey then .ask
  else .block

theorem an_older_refusal_is_reported (o : RefusalObservation) (fresh : o.fresh = true)
    (older : o.olderStore = true) : refusalAction o = .block := by
  simp [refusalAction, fresh, older]

end InstallRepair

namespace InstallRepair

inductive ClientBinding where
  | boundToHome | unbound | orphaned
  deriving Repr, DecidableEq, Lean.ToJson

structure ClientObservation where
  reviewed : Bool
  configuredMatches : Bool
  configuredSameDid : Bool
  reviewedHomeSameDid : Bool
  reviewedHomePresent : Bool
  reviewedInitPresent : Bool
  staleLocalPeer : Bool
  deriving Repr, DecidableEq, Lean.ToJson

def clientBinding (o : ClientObservation) : ClientBinding :=
  if !o.reviewed then
    if o.staleLocalPeer then .orphaned else .unbound
  else if o.configuredMatches then .boundToHome
  else if o.configuredSameDid || o.reviewedHomeSameDid then .unbound
  else if o.reviewedHomePresent then .orphaned
  else if o.reviewedInitPresent then .unbound
  else .orphaned

end InstallRepair

namespace InstallRepair

/-- Whether this machine may create its CLI client identity for a home. A home whose setup
finished has a Node owned by the identity that set it up: ACP lets only that owner, or an
administrator it granted, read its sessions or grant another identity. The onboarding test of #3670
deleted the client state; the next run minted a new key silently, and every session of the home was
hidden from it. So only a home not yet set up gets a new identity; a set-up home without one is
refused with the calls that restore or replace it. -/
structure ClientIdentityObservation where
  keyPresent : Bool
  settingsPresent : Bool
  pending : Bool
  deriving Repr, DecidableEq, Lean.ToJson

inductive ClientIdentityAction where
  | load | create | refuse
  deriving Repr, DecidableEq, Lean.ToJson

def clientIdentityAction (o : ClientIdentityObservation) : ClientIdentityAction :=
  if o.keyPresent then .load
  else if o.settingsPresent && !o.pending then .refuse
  else .create

theorem a_set_up_home_never_gets_a_new_identity (o : ClientIdentityObservation)
    (settings : o.settingsPresent = true) (finished : o.pending = false) :
    clientIdentityAction o ≠ .create := by
  unfold clientIdentityAction
  split <;> simp_all

end InstallRepair

namespace InstallRepair

inductive CrashSampleKind where
  | unloaded | ready | running | exited
  deriving Repr, DecidableEq, Lean.ToJson

structure CrashSample where
  kind : CrashSampleKind
  counter : Nat
  clean : Bool
  deriving Repr, DecidableEq, Lean.ToJson

structure CrashState where
  baseline : Option Nat := none
  looping : Option Nat := none
  deriving Repr, DecidableEq, Lean.ToJson

def crashStep (state : CrashState) (sample : CrashSample) : CrashState :=
  match sample.kind with
  | .unloaded | .ready => {}
  | .running => state
  | .exited =>
    if sample.clean then {}
    else
      let baseline := match state.baseline with
        | some old => if old ≤ sample.counter then old else sample.counter
        | none => sample.counter
      let restarts := sample.counter - baseline
      { baseline := some baseline, looping := if restarts ≥ 2 then some restarts else none }

def crashTrace (samples : List CrashSample) : List CrashState :=
  (samples.foldl (fun (state, states) sample =>
    let next := crashStep state sample
    (next, states ++ [next])) ({}, [])).2

inductive StoreKind where
  | legacyRocksDb | legacyLark | unsupportedOrCorrupt | unknownLineage | foreignVersion
  | insecureKey | unencryptedStore | missingStoreKey | otherVersionSchema
  deriving Repr, DecidableEq, Lean.ToJson

def storeExitCode : StoreKind → Nat
  | .insecureKey => 66
  | .foreignVersion => 67
  | .missingStoreKey => 72
  | _ => 65

end InstallRepair

namespace InstallRepair

structure RecoveryObservation where
  candidateExposed : Bool
  sameBuild : Bool
  deriving Repr, DecidableEq, Lean.ToJson

def canRestore (o : RecoveryObservation) : Bool :=
  RuntimeReconcile.UpdateHandoff.canRestore .installed o.candidateExposed o.sameBuild

theorem exposed_changed_build_is_not_restored : canRestore ⟨true, false⟩ = false := by decide

theorem unexposed_changed_build_may_be_restored : canRestore ⟨false, false⟩ = true := by decide

end InstallRepair
