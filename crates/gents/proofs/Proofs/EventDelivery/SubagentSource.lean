import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.SubagentSource

structure ReservedChildBinding where
  child : Nat
  agent : Nat
  behavior : Nat
  parentRequest : Nat
  parentRequestDoc : Nat
  parentTool : Nat
  parentToolDoc : Nat
  payload : Nat
  workspace : Option Nat
  admission : Nat
  deriving DecidableEq, Repr

inductive MaterializationDecision where
  | created | replayed | conflict
  deriving DecidableEq, Repr

def ensureReservedChild (stored : List ReservedChildBinding)
    (candidate : ReservedChildBinding) : MaterializationDecision × List ReservedChildBinding :=
  match stored.filter (·.child == candidate.child) with
  | [] => (.created, stored ++ [candidate])
  | [existing] =>
      if existing == candidate then (.replayed, stored) else (.conflict, stored)
  | _ => (.conflict, stored)

theorem fresh_then_exact_replay_creates_once (stored : List ReservedChildBinding)
    (candidate : ReservedChildBinding)
    (habsent : stored.all (·.child != candidate.child) = true) :
    let first := ensureReservedChild stored candidate
    first.1 = .created ∧
      ensureReservedChild first.2 candidate = (.replayed, first.2) := by
  have hfilter : stored.filter (·.child == candidate.child) = [] := by
    induction stored with
    | nil => simp
    | cons value rest ih =>
        simp only [List.all_cons, Bool.and_eq_true] at habsent
        have hne : value.child ≠ candidate.child := by
          simpa using habsent.1
        simp [hne, ih habsent.2]
  simp [ensureReservedChild, hfilter]

theorem conflict_preserves_stored (stored : List ReservedChildBinding)
    (candidate : ReservedChildBinding)
    (hconflict : (ensureReservedChild stored candidate).1 = .conflict) :
    (ensureReservedChild stored candidate).2 = stored := by
  simp only [ensureReservedChild] at hconflict ⊢
  split <;> simp_all
  split <;> simp_all

structure ReservedChildCase where
  name : String
  stored : List ReservedChildBinding
  candidate : ReservedChildBinding
  expectedDecision : MaterializationDecision
  expectedCount : Nat
  deriving Repr

def baseBinding : ReservedChildBinding :=
  ⟨42, 1, 2, 3, 30, 4, 40, 5, some 6, 7⟩

def reservedChildCases : List ReservedChildCase :=
  [ ⟨"fresh_reserved_child", [], baseBinding, .created, 1⟩
  , ⟨"exact_reserved_child_replay", [baseBinding], baseBinding, .replayed, 1⟩
  , ⟨"conflicting_physical_lineage",
      [baseBinding], { baseBinding with parentToolDoc := 41 }, .conflict, 1⟩
  , ⟨"conflicting_payload",
      [baseBinding], { baseBinding with payload := 8 }, .conflict, 1⟩
  , ⟨"conflicting_workspace",
      [baseBinding], { baseBinding with workspace := some 9 }, .conflict, 1⟩
  , ⟨"conflicting_admission",
      [baseBinding], { baseBinding with admission := 10 }, .conflict, 1⟩
  , ⟨"physical_twins_fail_closed", [baseBinding, baseBinding], baseBinding, .conflict, 2⟩
  ]

theorem reservedChildCases_derive : ∀ c ∈ reservedChildCases,
    (ensureReservedChild c.stored c.candidate).1 = c.expectedDecision ∧
    (ensureReservedChild c.stored c.candidate).2.length = c.expectedCount := by
  decide

def subagentSourceSrc : SourceInstance :=
  { name := "SubagentSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := 1
  }

theorem subagentSourceSrc_rescanBoundedBy_pos :
    0 < subagentSourceSrc.rescanBoundedBy := by
  decide

theorem O1_orphan_child_materialization
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair subagentSourceSrc actions ∧
      d ∈ w'.handled :=
  D1_delivery_convergence
    subagentSourceSrc w₀ d h_persisted h_unprocessed
    subagentSourceSrc_rescanBoundedBy_pos

end EventDelivery.SubagentSource
