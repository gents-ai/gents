import Proofs.Runtime.RuntimeReconcile.StartupReadiness
import Proofs.Tools.ToolPolicy.PluginPublication

namespace RuntimeReconcile.SurfacePublication

/-- Classify a transient publication failure. This classification alone is
NOT permission to retain authority: deferAuthority additionally requires the
epoch budget. At exhaustion the latest proposal must be published. -/
def keepPrevious (busy : Bool) : Bool := busy

theorem busy_is_transient : keepPrevious true = true := by decide

theorem permanent_failure_does_not_keep_stale_authority :
    keepPrevious false = false := by decide

/-- Publication contention can outlast the generic document-visibility settle
window; dropping its retry would strand the old generation after rollback
because a lock release itself emits no database event. -/
def retryProposal (busy withinSettleWindow : Bool) : Bool := busy || withinSettleWindow

theorem busy_retries_after_settle_window : retryProposal true false = true := by decide

end RuntimeReconcile.SurfacePublication

namespace RuntimeReconcile.SurfacePublication

/-- Busy deferral consumes one total epoch budget. Exhaustion publishes the
latest authority even if plugin agents must be demoted; retries then continue
without restoring a revoked grant. Neither events nor retry resets the epoch. -/
def deferAuthority (busy budgetRemaining : Bool) : Bool := busy && budgetRemaining

theorem expired_never_defers (busy : Bool) : deferAuthority busy false = false := by
  simp [deferAuthority]

end RuntimeReconcile.SurfacePublication
