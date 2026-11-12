import Lean
import Proofs.Runtime.RuntimeReconcile.UpdateHandoff

namespace InstallRepair

inductive HomeState where
  | absent | healthy | interrupted | unreadableSettings | missingSettings
  | missingStoreKey | olderStore | newerStore
  deriving Repr, DecidableEq, Lean.ToJson

inductive Version where
  | unknown | older | same | newer
  deriving Repr, DecidableEq, Lean.ToJson

inductive Step where
  | drain | backup | stop | coldBackup | updateService | archiveHome | start | health | rollback
  deriving Repr, DecidableEq, Lean.ToJson

inductive Blocker where
  | none | newerVersion | explicitHome | busy | keychain | foreignProcess | unreadableHome
  deriving Repr, DecidableEq, Lean.ToJson

structure Observation where
  home : HomeState
  version : Version
  explicitHome : Bool
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
  deriving Repr, DecidableEq, Lean.ToJson

structure Plan where
  steps : List Step
  blocker : Blocker
  needsSetup : Bool
  deriving Repr, DecidableEq, Lean.ToJson

def needsArchive : HomeState → Bool
  | .interrupted | .unreadableSettings | .missingSettings | .missingStoreKey | .olderStore => true
  | _ => false

def plan (o : Observation) : Plan := Id.run do
  if o.version == .newer || o.home == .newerStore then
    return ⟨[], .newerVersion, false⟩
  if !o.readable then return ⟨[], .unreadableHome, false⟩
  if o.busy then return ⟨[], .busy, false⟩
  if o.keychainLocked then return ⟨[], .keychain, false⟩
  if o.foreignProcess then return ⟨[], .foreignProcess, false⟩
  if needsArchive o.home && o.explicitHome then return ⟨[], .explicitHome, false⟩
  let archive := needsArchive o.home
  let setup := archive || o.home == .absent
  let update := o.servicePresent && !o.managedUpdate &&
    (o.version == .older || !o.serviceProgramPresent || !o.serviceSameHome)
  let stop := o.serviceRunning && (archive || o.home == .absent || update)
  let drain := stop && o.home == .healthy
  let restart := update && o.serviceRunning && !setup
  return ⟨(if drain then (if !o.drainSupported then [.backup] else [.drain]) else []) ++
    (if stop then [.stop] else []) ++
    (if update && o.home == .healthy then [.coldBackup] else []) ++
    (if archive then [.archiveHome] else []) ++
    (if update then [.updateService] else []) ++
    (if restart then [.start, .health] else []), .none, setup⟩

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
  ⟨.healthy, .same, false, false, false, false, true, true, running, true, true, false, true⟩

theorem healthy_repair_is_noop (running : Bool) :
    plan (healthy running) = ⟨[], .none, false⟩ := by
  cases running <;> decide

theorem older_running_backup_precedes_stop :
    plan { (healthy true) with version := .older, drainSupported := false } =
      ⟨[.backup, .stop, .coldBackup, .updateService, .start, .health], .none, false⟩ := by decide

theorem modern_running_drains_before_cold_backup :
    plan { (healthy true) with version := .older } =
      ⟨[.drain, .stop, .coldBackup, .updateService, .start, .health], .none, false⟩ := by decide

theorem newer_store_is_untouched (o : Observation) (h : o.home = .newerStore) :
    plan o = ⟨[], .newerVersion, false⟩ := by simp [plan, h, Id.run]

theorem newer_node_is_untouched (o : Observation) (h : o.version = .newer) :
    plan o = ⟨[], .newerVersion, false⟩ := by simp [plan, h, Id.run]

end InstallRepair

namespace InstallRepair

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
  deriving Repr, DecidableEq, Lean.ToJson

def classifyHome (o : HomeObservation) : HomeState :=
  if !o.present || (!o.ownedEntries && !o.bootstrapConsumed) then .absent
  else if o.pending || (o.bootstrapConsumed && !o.settingsPresent) then .interrupted
  else if !o.settingsReadable then .unreadableSettings
  else if !o.settingsPresent then
    if !o.dataPresent && !o.keysPresent then .absent else .missingSettings
  else if o.missingKeyRefusal then .missingStoreKey
  else .healthy

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
  deriving Repr, DecidableEq, Lean.ToJson

inductive ServiceAction where
  | move | stop | reinstall | switch | refreshLog | restart
  deriving Repr, DecidableEq, Lean.ToJson

structure ServiceStep where
  action : ServiceAction
  startAfter : Bool := false
  replaceProgram : Bool := false
  answered : Bool := false
  deriving Repr, DecidableEq, Lean.ToJson

def servicePlan (s : ServiceObservation) : List ServiceStep := Id.run do
  if !s.definitionPresent then return []
  if !s.sameHome then
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

structure RefusalObservation where
  fresh : Bool
  missingKey : Bool
  explicitHome : Bool
  olderStore : Bool
  newerStore : Bool
  deriving Repr, DecidableEq, Lean.ToJson

inductive RefusalAction where
  | ignore | ask | archive | block
  deriving Repr, DecidableEq, Lean.ToJson

def refusalAction (o : RefusalObservation) : RefusalAction :=
  if !o.fresh then .ignore
  else if o.newerStore then .block
  else if o.missingKey || o.olderStore then
    if o.explicitHome then .ask else .archive
  else .block

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
