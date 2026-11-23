namespace ToolPolicy.PluginPublication

inductive Phase where
  | staging | published
  deriving DecidableEq, Repr

inductive Event where
  | localFailure | localPublished | notificationSucceeded | notificationFailed | cancelled
  deriving DecidableEq, Repr

/-- Local plugin records and home pins are the installation authority. The
node's plugin-store record is an invalidation projection, not execution authority.
Local publication owns exclusive store access through staging and rollback.
After publication, notification can fail or lose its observer after committing;
neither outcome permits reverting the published installation. Process crashes
and filesystem failures during restoration require retry/reconciliation from the
local store and are not successful rollback observations in this model. -/
def step (phase : Phase) (event : Event) : Phase :=
  match phase, event with
  | .staging, .localPublished => .published
  | _, _ => phase

def run (events : List Event) : Phase := events.foldl step .staging

def installedNew (events : List Event) : Bool := run events == .published

theorem published_is_retained (event : Event) : step .published event = .published := by
  cases event <;> rfl

theorem notification_failure_cannot_undo_publication :
    installedNew [.localPublished, .notificationFailed] = true := rfl

theorem cancelled_observer_cannot_undo_publication :
    installedNew [.localPublished, .cancelled] = true := rfl

theorem rejected_local_stage_preserves_previous :
    installedNew [.localFailure] = false := rfl

/-- A process holding the store's exclusive publication lock exposes no record
to a reader. Only that typed contention is retried, and only while the
construction's original deadline is ahead: a busy observation never extends
it, and a retry never changes the selected coordinate or digest. -/
def retry (busy : Bool) (now deadline : Nat) : Bool := busy && decide (now < deadline)

/-- When the next attempt runs: one poll interval later, but never after the
deadline. -/
def nextAttempt (now deadline poll : Nat) : Nat := min deadline (now + poll)

theorem a_permanent_failure_never_retries (now deadline : Nat) :
    retry false now deadline = false := by simp [retry]

theorem an_expired_deadline_never_retries (busy : Bool) (now deadline : Nat)
    (h : deadline ≤ now) : retry busy now deadline = false := by
  have expired : ¬ now < deadline := Nat.not_lt.mpr h
  simp [retry, expired]

theorem busy_before_the_deadline_retries (now deadline : Nat) (h : now < deadline) :
    retry true now deadline = true := by simp [retry, h]

theorem the_next_attempt_never_passes_the_deadline (now deadline poll : Nat) :
    nextAttempt now deadline poll ≤ deadline := Nat.min_le_left _ _

theorem a_retry_makes_progress (now deadline poll : Nat) (h : now < deadline) (hp : 0 < poll) :
    now < nextAttempt now deadline poll :=
  Nat.lt_min.mpr ⟨h, by omega⟩

end ToolPolicy.PluginPublication
