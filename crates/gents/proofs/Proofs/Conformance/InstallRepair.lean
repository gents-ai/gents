import Proofs.Clients.InstallRepair
import Proofs.Conformance.Family

namespace Conformance.InstallRepair

open _root_.InstallRepair

private def bit (n k : Nat) : Bool := (n / 2 ^ k) % 2 == 1

private def homes : List HomeState :=
  [.absent, .healthy, .interrupted, .unreadableSettings, .missingSettings,
   .missingStoreKey, .olderStore, .newerStore]

private def versions : List Version := [.unknown, .older, .same, .newer]

@[conformance_family]
def classification : Conformance.Family HomeObservation HomeState where
  inputs := (List.range 512).map fun n =>
    ⟨bit n 0, bit n 1, bit n 2, bit n 3, bit n 4, bit n 5, bit n 6, bit n 7, bit n 8⟩
  model := classifyHome

@[conformance_family]
def plans : Conformance.Family Observation Plan where
  inputs := homes.flatMap fun home => versions.flatMap fun version =>
    (List.range 2048).map fun n =>
      ⟨home, version, bit n 0, bit n 1, bit n 2, bit n 3, bit n 4,
       bit n 5, bit n 6, bit n 7, bit n 8, bit n 9, bit n 10⟩
  model := plan

@[conformance_family]
def servicePlans : Conformance.Family ServiceObservation (List ServiceStep) where
  inputs := versions.flatMap fun version =>
    [ServedNode.none, .same, .different].flatMap fun serving =>
      (List.range 4096).map fun n =>
        ⟨bit n 0, bit n 1, bit n 2, bit n 3, bit n 4, bit n 5,
         bit n 6, bit n 7, bit n 8, bit n 9, version, bit n 10, bit n 11, serving⟩
  model := servicePlan

@[conformance_family]
def refusals : Conformance.Family RefusalObservation RefusalAction where
  inputs := (List.range 32).map fun n =>
    ⟨bit n 0, bit n 1, bit n 2, bit n 3, bit n 4⟩
  model := refusalAction

end Conformance.InstallRepair

namespace Conformance.InstallRepair
open _root_.InstallRepair

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
