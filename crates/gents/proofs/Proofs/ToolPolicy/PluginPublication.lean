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

end ToolPolicy.PluginPublication
