import Proofs.Clients.RepairBackups
import Proofs.Conformance.Family

namespace Conformance.RepairBackups

open _root_.RepairBackups

private def programs : List (Option Nat) := [none, some 0, some 1]

/-- Backups newest first: the highest id is the newest. -/
private def history (n : Nat) : List Backup :=
  (List.range n).reverse.map fun id => Backup.mk id (some (id % 2))

@[conformance_family]
def retentions : Conformance.Family Retention (List Nat) where
  inputs := (List.range 7).flatMap fun n =>
    (none :: (List.range (n + 1)).map some).map fun inUse => Retention.mk (history n) inUse
  model := fun r => (retained r).map (·.id)

/-- What a new backup of `program` decides against the backups already kept. -/
structure Sharing where
  kept : List Backup
  program : Option Nat
  deriving Repr, DecidableEq, Lean.ToJson

private def keptLists : List (List Backup) :=
  [[]] ++ programs.map (fun a => [Backup.mk 0 a]) ++
  programs.flatMap (fun a => programs.map fun b => [Backup.mk 1 a, Backup.mk 0 b]) ++
  programs.flatMap (fun a => programs.flatMap fun b => programs.map fun c =>
    [Backup.mk 2 a, Backup.mk 1 b, Backup.mk 0 c])

@[conformance_family]
def shares : Conformance.Family Sharing (Option Nat) where
  inputs := keptLists.flatMap fun kept => (programs ++ [some 2]).map fun program => Sharing.mk kept program
  model := fun s => sharedWith s.kept s.program

end Conformance.RepairBackups
