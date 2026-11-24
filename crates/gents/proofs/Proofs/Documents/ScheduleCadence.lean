/-! Advancing a schedule Trigger's cursor (`next_run_at`) once a tick fires the
due occurrence. Times are seconds since the epoch; `next t` is the cadence's
first occurrence strictly after `t`.

`fires` is the occurrences a tick sequence offers when each fire's cursor write
lands before the next tick reads the cursor. Production does not keep that
premise (the write follows the dispatch, in its own task, and can lag, fail or
be lost with the process), so the cursor is an optimization: a re-offered
occurrence answers with its one request because the request's identity is the
occurrence's (`Triggers.ScheduleOccurrence.no_occurrence_fires_twice`). -/

namespace ScheduleCadence

/-- How a tick that arrives after one or more occurrences advances the cursor. -/
inductive MissedRunPolicy where
  | catchUp
  | latestOnly
  deriving DecidableEq, Repr

/-- An interval cadence advances from the occurrence it fired, so a late or
missed tick leaves every missed occurrence due and later ticks fire them one at
a time; a cron cadence (whose only missed-run policy is latest-only) advances
from the tick's own time, so the occurrences a late tick missed collapse into
its one fire. -/
def advanceFrom (policy : MissedRunPolicy) (due now : Nat) : Nat :=
  match policy with
  | .catchUp => due
  | .latestOnly => now

def advance (next : Nat → Nat) (policy : MissedRunPolicy) (due now : Nat) : Nat :=
  next (advanceFrom policy due now)

/-- The occurrences a sequence of ticks fires, starting from the cursor `due`. -/
def fires (next : Nat → Nat) (policy : MissedRunPolicy) : Nat → List Nat → List Nat
  | _, [] => []
  | due, now :: later =>
    if due ≤ now then due :: fires next policy (advance next policy due now) later
    else fires next policy due later

theorem advance_passes_fired_occurrence (next : Nat → Nat) (policy : MissedRunPolicy)
    (due now : Nat) (increasing : ∀ t, t < next t) (fired : due ≤ now) :
    due < advance next policy due now := by
  cases policy
  · exact increasing due
  · exact Nat.lt_of_le_of_lt fired (increasing now)

theorem fires_at_or_after (next : Nat → Nat) (policy : MissedRunPolicy)
    (increasing : ∀ t, t < next t) :
    ∀ (ticks : List Nat) (due k : Nat), k ∈ fires next policy due ticks → due ≤ k := by
  intro ticks
  induction ticks with
  | nil => intro due k h; simp [fires] at h
  | cons now later ih =>
    intro due k h
    simp only [fires] at h
    split at h
    · rename_i fired
      rcases List.mem_cons.mp h with rfl | h
      · exact Nat.le_refl _
      · exact Nat.le_of_lt (Nat.lt_of_lt_of_le
          (advance_passes_fired_occurrence next policy due now increasing fired) (ih _ k h))
    · exact ih due k h

/-- Fired occurrences strictly increase. -/
theorem fires_strictly_increase (next : Nat → Nat) (policy : MissedRunPolicy)
    (increasing : ∀ t, t < next t) :
    ∀ (ticks : List Nat) (due : Nat), (fires next policy due ticks).Pairwise (· < ·) := by
  intro ticks
  induction ticks with
  | nil => intro due; simp [fires]
  | cons now later ih =>
    intro due
    simp only [fires]
    split
    · rename_i fired
      refine List.Pairwise.cons (fun k hk => ?_) (ih _)
      exact Nat.lt_of_lt_of_le (advance_passes_fired_occurrence next policy due now increasing fired)
        (fires_at_or_after next policy increasing later _ k hk)
    · exact ih due

/-- With every cursor write landing before the next tick, no tick sequence,
however late or repeated, offers one occurrence twice. -/
theorem cursor_fires_each_occurrence_once (next : Nat → Nat) (policy : MissedRunPolicy)
    (increasing : ∀ t, t < next t) (ticks : List Nat) (due : Nat) :
    (fires next policy due ticks).Nodup :=
  (fires_strictly_increase next policy increasing ticks due).imp Nat.ne_of_lt

/-- An interval cadence whose host was away for `n` periods catches up: `n`
ticks at the late time fire every missed occurrence, in order, once each. -/
theorem late_interval_ticks_catch_up_every_occurrence (period due now n : Nat)
    (late : due + n * period ≤ now) :
    fires (· + period) .catchUp due (List.replicate n now) =
      (List.range n).map (fun i => due + i * period) := by
  induction n generalizing due with
  | zero => simp [fires]
  | succ n ih =>
    have fired : due ≤ now := by
      have := Nat.le_add_right due ((n + 1) * period); omega
    have rest : due + period + n * period ≤ now := by
      rw [Nat.succ_mul] at late; omega
    simp only [List.replicate_succ, fires, if_pos fired, advance, advanceFrom]
    rw [ih (due + period) rest, List.range_succ_eq_map, List.map_cons, List.map_map]
    congr 1
    · simp
    · apply List.map_congr_left
      intro i _
      simp only [Function.comp, Nat.succ_mul]
      omega

/-- A latest-only cadence fires a late tick's due occurrence once and moves past
the tick's time: the occurrences it missed are never fired. -/
theorem late_latest_only_tick_skips_missed_occurrences (next : Nat → Nat) (due now : Nat)
    (increasing : ∀ t, t < next t) (fired : due ≤ now) :
    now < advance next .latestOnly due now ∧
      fires next .latestOnly due [now, now] = [due] := by
  have past : now < next now := increasing now
  refine ⟨past, ?_⟩
  simp [fires, fired, advance, advanceFrom, Nat.not_le_of_lt past]

end ScheduleCadence
