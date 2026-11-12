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
    (List.range 1024).map fun n =>
      ⟨home, version, bit n 0, bit n 1, bit n 2, bit n 3, bit n 4,
       bit n 5, bit n 6, bit n 7, bit n 8, bit n 9⟩
  model := plan

@[conformance_family]
def servicePlans : Conformance.Family ServiceObservation (List ServiceStep) where
  inputs := versions.flatMap fun version => (List.range 16384).map fun n =>
    ⟨bit n 0, bit n 1, bit n 2, bit n 3, bit n 4, bit n 5,
     bit n 6, bit n 7, bit n 8, bit n 9, version, bit n 10, bit n 11,
     bit n 12, bit n 13⟩
  model := servicePlan

@[conformance_family]
def refusals : Conformance.Family RefusalObservation RefusalAction where
  inputs := (List.range 32).map fun n =>
    ⟨bit n 0, bit n 1, bit n 2, bit n 3, bit n 4⟩
  model := refusalAction

end Conformance.InstallRepair
