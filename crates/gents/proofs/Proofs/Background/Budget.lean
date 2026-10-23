import Proofs.Basic

namespace Background

/-- Admitted background rows one physical parent request may own; this limit
is runtime-local under the one-active-runtime-per-node convention. -/
def maxBackgroundedPerParent : Nat := 8

/-- The native owner counts running background rows and reserved spawn children,
excluding accepted calls that have not reserved capacity. It serializes that
count with the reservation or transition to running in one transaction. -/
def admitBackground (liveCount : Nat) : Option Nat :=
  if liveCount < maxBackgroundedPerParent then some (liveCount + 1) else none

/-- Pending accepted calls do not occupy capacity. A child reservation or a
running call does; terminal rows release their slot and remain in history. -/
inductive BackgroundRowState where
  | pending
  | admitted
  | terminal
  deriving BEq, DecidableEq, Repr

def admittedBackgroundRows (rows : List BackgroundRowState) : Nat :=
  rows.filter (· == .admitted) |>.length

/-- Admit the first pending row against the already-admitted rows. A rejected
pending row becomes terminal. -/
def admitFirstPendingFrom (rows : List BackgroundRowState) (liveCount : Nat) :
    Option (List BackgroundRowState × Bool) :=
  match rows with
  | [] => none
  | .pending :: rest =>
      match admitBackground liveCount with
      | some _ => some (.admitted :: rest, true)
      | none => some (.terminal :: rest, false)
  | row :: rest =>
      (admitFirstPendingFrom rest liveCount).map fun (updated, admitted) => (row :: updated, admitted)

def admitFirstPending (rows : List BackgroundRowState) :
    Option (List BackgroundRowState × Bool) :=
  admitFirstPendingFrom rows (admittedBackgroundRows rows)

def pendingBackgroundRows : Nat → List BackgroundRowState
  | 0 => []
  | n + 1 => .pending :: pendingBackgroundRows n

def admitBackgroundBatch : List BackgroundRowState → Nat → List BackgroundRowState
  | rows, 0 => rows
  | rows, n + 1 =>
      match admitFirstPending rows with
      | none => rows
      | some (updated, _) => admitBackgroundBatch updated n

theorem ten_pending_background_calls_admit_eight :
    admittedBackgroundRows (admitBackgroundBatch (pendingBackgroundRows 10) 10) =
      maxBackgroundedPerParent := by
  decide

theorem terminal_background_row_releases_capacity :
    admittedBackgroundRows [.admitted, .terminal, .admitted] = 2 := by
  decide

theorem rejected_background_row_is_terminal :
    (admitFirstPending (List.replicate 8 .admitted ++ [.pending])).getD ([], false) =
      (List.replicate 8 .admitted ++ [.terminal], false) := by
  decide

theorem admitted_background_count_bounded (before after : Nat)
    (h : admitBackground before = some after) :
    after = before + 1 ∧ after ≤ maxBackgroundedPerParent := by
  unfold admitBackground at h
  split at h
  · cases h
    exact ⟨rfl, by omega⟩
  · contradiction

theorem full_background_budget_rejected (liveCount : Nat)
    (h : maxBackgroundedPerParent ≤ liveCount) : admitBackground liveCount = none := by
  simp [admitBackground, Nat.not_lt.mpr h]

/-- A bounded contention trace records failed transaction attempts followed by
one committed receipt. Exhaustion leaves the receipt unpublished; it never
turns a storage conflict into a tool refusal. -/
def receiptCommits : List Bool → Nat → Bool
  | [], _ => false
  | _, 0 => false
  | committed :: rest, attempts + 1 =>
      committed || receiptCommits rest attempts

theorem receipt_commits_after_bounded_conflicts (conflicts : Nat) :
    receiptCommits (List.replicate conflicts false ++ [true]) (conflicts + 1) = true := by
  induction conflicts with
  | zero => rfl
  | succ n ih => simpa [List.replicate_succ, receiptCommits] using ih

/-- The concurrent admission regression has two overflow calls as well as the
admitted cohort. Its receipts share the canonical publication transaction. This
is a finite qualification bound, not a guarantee under unbounded external writes. -/
def receiptContentionAttempts : Nat := maxBackgroundedPerParent + 2

theorem admission_cohort_receipt_commits :
    receiptCommits (List.replicate (receiptContentionAttempts - 1) false ++ [true])
      receiptContentionAttempts = true := by
  decide

end Background
