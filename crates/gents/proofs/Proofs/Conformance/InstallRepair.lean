import Proofs.Clients.InstallRepair
import Proofs.Conformance.Family

namespace Conformance.InstallRepair

open _root_.InstallRepair

private def bit (n k : Nat) : Bool := (n / 2 ^ k) % 2 == 1

private def homes : List HomeState :=
  [.absent, .healthy, .interrupted, .unreadableSettings, .missingSettings,
   .missingStoreKey, .olderStore, .newerStore, .insecureKey, .corruptStore, .unfinishedSetup]

private def versions : List Version := [.unknown, .older, .same, .newer]

private def verdicts : List StoreVerdict :=
  [.fine, .older, .newer, .corrupt, .insecureKey, .missingKey]

@[conformance_family]
def classification : Conformance.Family HomeObservation HomeState where
  inputs := verdicts.flatMap fun store => (List.range 1024).map fun n =>
    ⟨bit n 0, bit n 1, bit n 2, bit n 3, bit n 4, bit n 5, bit n 6, bit n 7, bit n 8, bit n 9,
     store⟩
  model := classifyHome

private def servedHomes : List (Bool × Operation) :=
  [(true, .run), (false, .repoint), (true, .repoint)]

private def answers : List (Bool × Answer) :=
  [(false, .none), (false, .setAside), (true, .none), (true, .setAside), (true, .continueSetup)]

@[conformance_family]
def plans : Conformance.Family Observation Plan where
  inputs := homes.flatMap fun home => versions.flatMap fun version =>
    answers.flatMap fun (interactive, answer) =>
      ((List.range 256).map fun n =>
        ⟨home, version, interactive, answer, bit n 0, false, false, bit n 1, bit n 2, bit n 3,
         bit n 4, bit n 5, bit n 6, bit n 7, false, .run⟩) ++
      ((List.range 4).map fun n =>
        ⟨home, version, interactive, answer, false, bit n 0, bit n 1, true, true, true,
         true, true, false, true, false, .run⟩) ++
      (if version == .same || version == .older then
        servedHomes.flatMap fun (present, operation) => (List.range 32).map fun n =>
          ⟨home, version, interactive, answer, bit n 0, false, false, true, true, bit n 1,
           bit n 2, false, bit n 3, bit n 4, present, operation⟩
       else [])
  model := plan

/-- Nothing held under the label: the plans of the definition path alone. -/
private def free : ManagerRecord := ⟨.none, false, false, false⟩

/-- Every record the service manager can hold under the label. -/
private def records : List ManagerRecord :=
  [RecordedHome.none, .chosen, .other].flatMap fun recorded => (List.range 8).map fun n =>
    ⟨recorded, bit n 0, bit n 1, bit n 2⟩

/-- The record the manager reports, replayed against the real observation of it. -/
@[conformance_family]
def serviceHolders : Conformance.Family ManagerRecord Holder where
  inputs := records
  model := holder

@[conformance_family]
def servicePlans : Conformance.Family ServiceObservation (List ServiceStep) where
  inputs := (versions.flatMap fun version =>
    [ServedNode.none, .same, .different].flatMap fun serving =>
      ((List.range 4096).map fun n =>
        ⟨bit n 0, bit n 1, bit n 2, bit n 3, bit n 4, bit n 5,
         bit n 6, bit n 7, bit n 8, bit n 9, version, bit n 10, bit n 11, serving,
         false, .run, free⟩) ++
      servedHomes.flatMap fun (present, operation) => (List.range 1024).map fun n =>
        ⟨true, false, bit n 0, bit n 1, bit n 2, bit n 3,
         bit n 4, bit n 5, bit n 6, bit n 7, version, bit n 8, bit n 9, serving,
         present, operation, free⟩) ++
    -- A job is held, possibly for another existing home, and the asking `HOME` may hold no
    -- definition: the 2026-11-13 case among them.
    records.flatMap fun record => [Operation.run, .repoint].flatMap fun operation =>
      [ServedNode.none, .different].flatMap fun serving => (List.range 64).map fun n =>
        ⟨bit n 0, bit n 0 && bit n 1, bit n 2, bit n 3, true, true, bit n 4, bit n 4 && bit n 5,
         false, false, .same, false, false, serving, !(bit n 1), operation, record⟩
  model := servicePlan

/-- One machine seen by both planners: the plan's service fields are the service observation's. -/
@[conformance_family]
def settlePlans : Conformance.Family SettleObservation (List ServiceStep) where
  inputs := [HomeState.absent, .healthy, .interrupted, .missingStoreKey, .olderStore,
      .unfinishedSetup].flatMap fun home =>
    answers.flatMap fun (interactive, answer) =>
      [(false, Operation.run), (true, .run), (true, .repoint)].flatMap fun (present, operation) =>
        (List.range 32).map fun n =>
          ⟨⟨home, .same, interactive, answer, false, false, false, true, bit n 0, bit n 3,
            true, bit n 0 && bit n 1, false, false, present, operation⟩,
           ⟨bit n 0, bit n 0 && bit n 1, true, home == .healthy || home == .unfinishedSetup, true, true,
            bit n 2, bit n 3, false, false, .same, false, false,
            if bit n 4 then .same else .none, present, operation,
            if bit n 4 then ⟨.chosen, bit n 0, true, true⟩ else free⟩⟩
  model := settleServicePlan

/-- The 2026-11-13 machine: the asking `HOME` holds no definition, and the user's node is held for
the user's own home from the user's own LaunchAgents. Whatever the chosen home and the answer, the
repair applies nothing to it. -/
@[conformance_family]
def settleBesideAnotherHome : Conformance.Family SettleObservation (List ServiceStep) where
  inputs := [HomeState.absent, .healthy, .interrupted, .missingStoreKey, .unfinishedSetup].flatMap
    fun home => answers.flatMap fun (interactive, answer) => (List.range 4).map fun n =>
      ⟨⟨home, .same, interactive, answer, false, false, false, true, false, bit n 0,
        true, false, false, false, false, .run⟩,
       ⟨false, false, true, home == .healthy || home == .unfinishedSetup, true, true,
        true, bit n 0, false, false, .same, false, false, .none, false, .run,
        ⟨.other, true, false, bit n 1⟩⟩⟩
  model := settleServicePlan

@[conformance_family]
def refusals : Conformance.Family RefusalObservation RefusalAction where
  inputs := (List.range 16).map fun n =>
    ⟨bit n 0, bit n 1, bit n 2, bit n 3⟩
  model := refusalAction

end Conformance.InstallRepair

namespace Conformance.InstallRepair
open _root_.InstallRepair

@[conformance_family]
def clientIdentities : Conformance.Family ClientIdentityObservation ClientIdentityAction where
  inputs := (List.range 8).map fun n => ⟨bit n 0, bit n 1, bit n 2⟩
  model := clientIdentityAction

private def setups : List HomeSetup :=
  [⟨false, false⟩, ⟨true, false⟩, ⟨true, true⟩]

@[conformance_family]
def clientCommands : Conformance.Family ClientCommand ClientIdentityAction where
  inputs := [HomeSelection.flag, .env, .remembered, .fallback].flatMap fun selection =>
    [false, true].flatMap fun keyPresent => setups.flatMap fun selected =>
      [⟨false, false⟩, ⟨true, false⟩].map fun other => ⟨selection, keyPresent, selected, other⟩
  model := commandIdentityAction

@[conformance_family]
def clientBindings : Conformance.Family ClientObservation ClientBinding where
  inputs := (List.range 128).map fun n =>
    ⟨bit n 0, bit n 1, bit n 2, bit n 3, bit n 4, bit n 5, bit n 6⟩
  model := clientBinding

end Conformance.InstallRepair

namespace Conformance.InstallRepair
open _root_.InstallRepair

private def crashSamples : List CrashSample :=
  [⟨.unloaded, 0, false⟩, ⟨.ready, 0, false⟩, ⟨.running, 0, false⟩] ++
  (List.range 6).flatMap fun counter => [false, true].map fun clean => ⟨.exited, counter, clean⟩

@[conformance_family]
def crashLoops : Conformance.Family (List CrashSample) (List CrashState) where
  inputs := crashSamples.flatMap fun a => crashSamples.flatMap fun b => crashSamples.map fun c => [a,b,c]
  model := crashTrace

@[conformance_family]
def storeExitCodes : Conformance.Family StoreKind Nat where
  inputs := [.legacyRocksDb, .legacyLark, .unsupportedOrCorrupt, .unknownLineage, .foreignVersion,
    .insecureKey, .unencryptedStore, .missingStoreKey, .otherVersionSchema]
  model := storeExitCode

end Conformance.InstallRepair

namespace Conformance.InstallRepair
open _root_.InstallRepair

@[conformance_family]
def recovery : Conformance.Family RecoveryObservation Bool where
  inputs := [false, true].flatMap fun exposed => [false, true].map fun same => ⟨exposed, same⟩
  model := canRestore

end Conformance.InstallRepair
