import Proofs.Shared.Basic

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


/-- A process holding the store's exclusive publication lock exposes no
record to a reader. Only that typed contention is retryable, and only while
this construction's original wait budget remains open. Neither retry nor
budget exhaustion changes the selected coordinate or digest. -/
def retry (busy budgetAvailable : Bool) : Bool := busy && budgetAvailable

theorem permanent_failure_never_retries (budgetAvailable : Bool) :
    retry false budgetAvailable = false := by simp [retry]

theorem exhausted_budget_never_retries (busy : Bool) :
    retry busy false = false := by simp [retry]

theorem busy_with_budget_waits : retry true true = true := by decide

end ToolPolicy.PluginPublication

namespace ToolPolicy.PluginPublication

/-- The native planner reads only explicitly named Tools IDs. Eligibility
within those documents requires a replacement shipped by this archive and an
existing pin. Selection is a query boundary, not a constant boolean model. -/
def movePin (replacement pinned : Bool) : Bool := replacement && pinned

theorem unshipped_pin_unchanged (pinned : Bool) :
    movePin false pinned = false := by simp [movePin]

theorem unpinned_selection_unchanged (replacement : Bool) :
    movePin replacement false = false := by simp [movePin]

/-- A constructed slot can execute only a digest in its current explicit
pin set. Refreshing a stale slot cannot widen that set to installed bytes. -/
def digestAuthorized (pinnedDigests : List Nat) (runningDigest : Nat) : Bool :=
  pinnedDigests.contains runningDigest

theorem revoked_digest_never_runs (digest : Nat) : digestAuthorized [] digest = false := by
  simp [digestAuthorized]

end ToolPolicy.PluginPublication
