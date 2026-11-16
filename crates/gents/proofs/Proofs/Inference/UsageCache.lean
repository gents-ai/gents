import Proofs.Inference.UsageBinding

/-! The runtime's usage cache. A write lands under the node and the binding it
names, and only while that binding is the one in force (UsageBinding). A landed
observation replaces the stored one only when it is not older, so the newer
observation wins whatever order the writes arrive in. A binding with nothing
stored reads as no observation: another runtime's cache starts empty, and a
backend whose connection changed reads under a binding nothing was written to.
Each window is reduced to its used percentage. -/

namespace UsageCache

structure Write where
  node : String
  bound : String
  current : String
  observedAt : Nat
  used : Nat
  deriving DecidableEq, Repr

structure Entry where
  node : String
  binding : String
  observedAt : Nat
  used : Nat
  deriving DecidableEq, Repr

abbrev Cache := List Entry

def empty : Cache := []

def Write.lands (write : Write) : Bool :=
  UsageBinding.lands (some write.bound) write.current

def stored (cache : Cache) (node binding : String) : Option Entry :=
  cache.find? fun entry => entry.node == node && entry.binding == binding

def keep (cache : Cache) (write : Write) : Cache :=
  if write.lands && (stored cache write.node write.bound).all
      (fun entry => decide (entry.observedAt ≤ write.observedAt)) then
    ({ node := write.node, binding := write.bound, observedAt := write.observedAt,
       used := write.used } : Entry) :: cache
  else cache

def record (writes : List Write) : Cache :=
  writes.foldl keep empty

def lookup (cache : Cache) (node binding : String) : Option Nat :=
  (stored cache node binding).map (·.used)

def firstWrite : Write :=
  { node := "runtime", bound := "row-1", current := "row-1", observedAt := 1000, used := 12 }

def lateWrite : Write :=
  { node := "runtime", bound := "row-1", current := "row-2", observedAt := 1001, used := 90 }

def newestWrite : Write :=
  { node := "runtime", bound := "conn-1", current := "conn-1", observedAt := 1000, used := 50 }

def oldestWrite : Write :=
  { node := "runtime", bound := "conn-1", current := "conn-1", observedAt := 999, used := 5 }

theorem cache_is_runtime_local_and_replacement_sign_ins_reject_late_responses :
    lookup (record [firstWrite]) "runtime" "row-1" = some 12 ∧
    lookup empty "runtime" "row-1" = none ∧
    lookup (record [firstWrite, lateWrite]) "runtime" "row-2" = none := by
  native_decide

theorem newer_observations_win_and_backend_edits_get_an_empty_cache :
    lookup (record [newestWrite, oldestWrite]) "runtime" "conn-1" = some 50 ∧
    lookup (record [oldestWrite, newestWrite]) "runtime" "conn-1" = some 50 ∧
    lookup (record [newestWrite, oldestWrite]) "other-runtime" "conn-1" = none ∧
    lookup (record [newestWrite, oldestWrite]) "runtime" "conn-2" = none := by
  native_decide

end UsageCache
