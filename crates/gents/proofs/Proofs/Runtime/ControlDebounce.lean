/-! When the runtime's control watcher reconciles after the changes that woke
it. A wake (a control row, a measured-health flip, or an access change on a
Vera home, `AccessWake`) schedules the reconcile a delay later, so a burst of
changes is reconciled once. Access changes on a busy Vera home arrive with
every settled write, unrelated to control documents, so a delay counted from
the last wake alone could postpone a pending change for as long as the home
stays busy. The interleaving is `tla/ControlDebounce.tla`. -/

namespace ControlDebounce

/-- Decision: the reconcile runs `delay` after the latest wake, but no later
than `ceiling` after the first wake it has not answered. Times are in
milliseconds. -/
def due (firstWake now delay ceiling : Nat) : Nat :=
  min (now + delay) (firstWake + ceiling)

theorem a_pending_change_is_reconciled_within_the_ceiling (firstWake now delay ceiling : Nat) :
    due firstWake now delay ceiling ≤ firstWake + ceiling :=
  Nat.min_le_right _ _

theorem a_quiet_burst_keeps_its_delay (firstWake now delay ceiling : Nat)
    (h : now + delay ≤ firstWake + ceiling) :
    due firstWake now delay ceiling = now + delay :=
  Nat.min_eq_left h

end ControlDebounce
