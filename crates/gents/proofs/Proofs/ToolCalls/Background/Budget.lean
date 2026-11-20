import Proofs.Shared.Basic

namespace Background

/-- Bound the live background calls one physical parent request can retain so
one request cannot monopolize node resources. This runtime-local budget relies
on the one-active-runtime-per-node convention. It is the host ceiling, not a
per-Agent setting: the store's sibling contention bound
(`receiptContentionAttempts`) derives from it, so an Agent's configured limit
only lowers the budget below it (`backgroundLimit_le_ceiling`). -/
def maxBackgroundedPerParent : Nat := 8

/-- The Agent's Tools `host.max_background_tool_calls`; unset is the ceiling.
The Tools validation owner admits only `validBackgroundLimit` values, and the
`min` keeps the ceiling for a value it would reject. -/
def backgroundLimit (configured : Option Nat) : Nat :=
  min (configured.getD maxBackgroundedPerParent) maxBackgroundedPerParent

/-- A configured `host.max_background_tool_calls` the Tools validation owner
admits. Zero would refuse every background call, which `background_enabled`
already expresses. -/
def validBackgroundLimit (configured : Nat) : Bool :=
  0 < configured && configured ≤ maxBackgroundedPerParent

theorem backgroundLimit_le_ceiling (configured : Option Nat) :
    backgroundLimit configured ≤ maxBackgroundedPerParent :=
  Nat.min_le_right _ _

theorem backgroundLimit_unset_is_ceiling :
    backgroundLimit none = maxBackgroundedPerParent := by
  decide

theorem valid_backgroundLimit_is_configured (configured : Nat)
    (h : validBackgroundLimit configured = true) :
    backgroundLimit (some configured) = configured := by
  simp only [validBackgroundLimit, Bool.and_eq_true, decide_eq_true_eq] at h
  simp [backgroundLimit, Nat.min_eq_left h.2]

/-- The native owner counts running background rows and reserved spawn children,
excluding accepted calls that have not reserved capacity, against the parent
Agent's resolved `limit`. It serializes that count with the reservation or
transition to running in one transaction. -/
def admitBackground (limit liveCount : Nat) : Option Nat :=
  if liveCount < limit then some (liveCount + 1) else none

/-- Recovery transfers an already-running physical invocation into the background
owner. It creates no execution; the transferred row continues to count when
subsequent admissions enforce the ordinary ceiling. -/
def recoverRunningBackground (liveCount : Nat) : Nat := liveCount + 1

theorem recovery_at_capacity_remains_counted (limit : Nat) :
    recoverRunningBackground limit = limit + 1 ∧
    admitBackground limit (recoverRunningBackground limit) = none := by
  simp [recoverRunningBackground, admitBackground]

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
def admitFirstPendingFrom (limit : Nat) (rows : List BackgroundRowState) (liveCount : Nat) :
    Option (List BackgroundRowState × Bool) :=
  match rows with
  | [] => none
  | .pending :: rest =>
      match admitBackground limit liveCount with
      | some _ => some (.admitted :: rest, true)
      | none => some (.terminal :: rest, false)
  | row :: rest =>
      (admitFirstPendingFrom limit rest liveCount).map fun (updated, admitted) =>
        (row :: updated, admitted)

def admitFirstPending (limit : Nat) (rows : List BackgroundRowState) :
    Option (List BackgroundRowState × Bool) :=
  admitFirstPendingFrom limit rows (admittedBackgroundRows rows)

def pendingBackgroundRows : Nat → List BackgroundRowState
  | 0 => []
  | n + 1 => .pending :: pendingBackgroundRows n

def admitBackgroundBatch (limit : Nat) :
    List BackgroundRowState → Nat → List BackgroundRowState
  | rows, 0 => rows
  | rows, n + 1 =>
      match admitFirstPending limit rows with
      | none => rows
      | some (updated, _) => admitBackgroundBatch limit updated n

theorem ten_pending_background_calls_admit_eight :
    admittedBackgroundRows
        (admitBackgroundBatch (backgroundLimit none) (pendingBackgroundRows 10) 10) =
      maxBackgroundedPerParent := by
  decide

theorem configured_limit_admits_only_its_count :
    admittedBackgroundRows
        (admitBackgroundBatch (backgroundLimit (some 3)) (pendingBackgroundRows 10) 10) = 3 := by
  decide

theorem terminal_background_row_releases_capacity :
    admittedBackgroundRows [.admitted, .terminal, .admitted] = 2 := by
  decide

theorem rejected_background_row_is_terminal :
    (admitFirstPending (backgroundLimit none)
        (List.replicate 8 .admitted ++ [.pending])).getD ([], false) =
      (List.replicate 8 .admitted ++ [.terminal], false) := by
  decide

theorem admitted_background_count_bounded (limit before after : Nat)
    (h : admitBackground limit before = some after) :
    after = before + 1 ∧ after ≤ limit := by
  unfold admitBackground at h
  split at h
  · cases h
    exact ⟨rfl, by omega⟩
  · contradiction

/-- Whatever an Agent configures, admission never exceeds the host ceiling the
sibling contention bound is sized for. -/
theorem admitted_background_within_ceiling (configured : Option Nat) (before after : Nat)
    (h : admitBackground (backgroundLimit configured) before = some after) :
    after ≤ maxBackgroundedPerParent :=
  Nat.le_trans (admitted_background_count_bounded _ before after h).2
    (backgroundLimit_le_ceiling configured)

theorem full_background_budget_rejected (limit liveCount : Nat)
    (h : limit ≤ liveCount) : admitBackground limit liveCount = none := by
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

/-- Attempts for a transaction that contends with the request's siblings. The
owned loop admits a turn's calls one at a time (a non-read call is dispatched
alone, and a read batch awaits each call's admission and receipt before the
next), so while one call's admission or receipt is in flight the only
concurrent writers of its request and session documents are its running
children, at most `maxBackgroundedPerParent` whatever the Agent configures
(`admitted_background_within_ceiling`), and the request's execution
lease renewal. Renewals of one lease are at least half its duration apart
(`RequestExecutionLease.consecutive_renewals_obey_cadence`), longer than this
attempt series for any lease of at least a few seconds, so at most one renewal
lands in it. Two more attempts outlast them; the second is the renewal's
(`tla/BackgroundAdmissionContention` `RenewalBudget`). Admitting sibling calls concurrently is outside this bound: each then contends
with every sibling commit and can exhaust it
(`tla/BackgroundAdmissionContention`, #3592). This is not a guarantee under
unbounded external writes. -/
def receiptContentionAttempts : Nat := maxBackgroundedPerParent + 2

theorem admission_cohort_receipt_commits :
    receiptCommits (List.replicate (receiptContentionAttempts - 1) false ++ [true])
      receiptContentionAttempts = true := by
  decide

end Background
