import Proofs.SessionHydration.State

/-!
# Receiver-side hydration progress

The server writes `served_doc_count` after delivery is accepted. The client
must not treat that write as completion: it counts unique locally merged
transcript documents and may only complete when that count covers the
server's denominator. Empty served sessions (`servedCount = 0`) complete
immediately.
-/

namespace SessionHydration

inductive ClientPhase where
  | idle
  | requested
  | serving
  | complete
  | failed
  deriving DecidableEq, Repr

structure ClientProgress where
  phase : ClientPhase := .idle
  mergedCount : Nat := 0
  servedCount : Option Nat := none
  deriving DecidableEq

def canComplete (mergedCount : Nat) (servedCount : Option Nat) : Bool :=
  match servedCount with
  | some n => decide (mergedCount ≥ n)
  | none => false

def mergeServed (prev next : Option Nat) : Option Nat :=
  match next with
  | some n => some n
  | none => prev

def observeCore (prev : ClientProgress) (merged : Nat) (served : Option Nat)
    (failed : Bool) : ClientProgress :=
  if failed || decide (prev.phase = .failed) then
    { phase := .failed, mergedCount := merged, servedCount := served }
  else if canComplete merged served then
    { phase := .complete, mergedCount := merged, servedCount := served }
  else if served.isSome || decide (merged > 0) || decide (prev.phase = .serving) then
    { phase := .serving, mergedCount := merged, servedCount := served }
  else
    { phase := .requested, mergedCount := merged, servedCount := served }

def observe (prev : ClientProgress) (mergedCount : Nat) (servedCount : Option Nat)
    (failed : Bool) : ClientProgress :=
  observeCore prev
    (max prev.mergedCount mergedCount)
    (mergeServed prev.servedCount servedCount)
    failed

theorem observeCore_mergedCount (prev : ClientProgress) (merged : Nat)
    (served : Option Nat) (failed : Bool) :
    (observeCore prev merged served failed).mergedCount = merged := by
  unfold observeCore
  split_ifs <;> rfl

theorem observe_mergedCount (prev : ClientProgress) (mergedCount : Nat)
    (servedCount : Option Nat) (failed : Bool) :
    (observe prev mergedCount servedCount failed).mergedCount =
      max prev.mergedCount mergedCount := by
  unfold observe
  exact observeCore_mergedCount _ _ _ _

theorem observe_merged_monotone (prev : ClientProgress) (mergedCount : Nat)
    (servedCount : Option Nat) (failed : Bool) :
    prev.mergedCount ≤ (observe prev mergedCount servedCount failed).mergedCount := by
  rw [observe_mergedCount]
  exact Nat.le_max_left _ _

theorem observe_complete_iff (prev : ClientProgress) (mergedCount : Nat)
    (servedCount : Option Nat)
    (hprev : prev.phase ≠ .failed) :
    (observe prev mergedCount servedCount false).phase = .complete ↔
      canComplete (max prev.mergedCount mergedCount)
        (mergeServed prev.servedCount servedCount) = true := by
  unfold observe observeCore
  have hnf : decide (prev.phase = .failed) = false := decide_eq_false_iff_not.mpr hprev
  simp [hnf]
  split_ifs <;> simp_all

theorem observe_cannot_complete_without_server (prev : ClientProgress)
    (mergedCount : Nat)
    (hprev : prev.phase ≠ .failed)
    (hserved : mergeServed prev.servedCount none = none) :
    (observe prev mergedCount none false).phase ≠ .complete := by
  intro hcomplete
  have hiff :=
    (observe_complete_iff prev mergedCount none hprev).mp hcomplete
  unfold canComplete at hiff
  simp [hserved] at hiff

end SessionHydration
