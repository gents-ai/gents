import Proofs.Inference.UsageCache
import Proofs.Conformance.Family

namespace Conformance.UsageCacheRuntime

open Lean

structure ReplacementInput where
  used : Nat
  late : Nat
  deriving ToJson

structure ReplacementOutput where
  sameRuntime : Option Nat
  otherRuntime : Option Nat
  replacement : Option Nat
  deriving ToJson

@[conformance_family]
def replacement : Conformance.Family ReplacementInput ReplacementOutput where
  inputs := [{ used := 12, late := 90 }, { used := 90, late := 12 }]
  model := fun i =>
    let first : UsageCache.Write :=
      { node := "runtime", bound := "row-1", current := "row-1", observedAt := 1000, used := i.used }
    let late : UsageCache.Write :=
      { node := "runtime", bound := "row-1", current := "row-2", observedAt := 1001, used := i.late }
    { sameRuntime := UsageCache.lookup (UsageCache.record [first]) "runtime" "row-1"
      otherRuntime := UsageCache.lookup UsageCache.empty "runtime" "row-1"
      replacement := UsageCache.lookup (UsageCache.record [first, late]) "runtime" "row-2" }

structure NewerInput where
  newerFirst : Bool
  deriving ToJson

structure NewerOutput where
  used : Option Nat
  otherNode : Option Nat
  afterEdit : Option Nat
  deriving ToJson

@[conformance_family]
def newer : Conformance.Family NewerInput NewerOutput where
  inputs := [{ newerFirst := true }, { newerFirst := false }]
  model := fun i =>
    let newest : UsageCache.Write :=
      { node := "runtime", bound := "conn-1", current := "conn-1", observedAt := 1000, used := 50 }
    let oldest : UsageCache.Write :=
      { node := "runtime", bound := "conn-1", current := "conn-1", observedAt := 999, used := 5 }
    let cache := UsageCache.record (if i.newerFirst then [newest, oldest] else [oldest, newest])
    { used := UsageCache.lookup cache "runtime" "conn-1"
      otherNode := UsageCache.lookup cache "other-runtime" "conn-1"
      afterEdit := UsageCache.lookup cache "runtime" "conn-2" }

end Conformance.UsageCacheRuntime
