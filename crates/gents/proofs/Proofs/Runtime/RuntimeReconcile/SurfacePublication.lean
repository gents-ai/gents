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

namespace RuntimeReconcile.SurfacePublication

/-- Stores cannot block construction of surfaces with no selected plugins. -/
def needsPublicationFence (hasPlugins : Bool) : Bool := hasPlugins

theorem plugin_free_never_waits : needsPublicationFence false = false := by decide

/-- A parked slot is not a runnable reusable executor, even when its
configuration fingerprints match a recovered proposal exactly. -/
def recreateSlot (configurationMatches parked : Bool) : Bool := !configurationMatches || parked

theorem parked_is_recreated (configurationMatches : Bool) : recreateSlot configurationMatches true = true := by
  simp [recreateSlot]

end RuntimeReconcile.SurfacePublication

namespace RuntimeReconcile.SurfacePublication

/-- A pending busy-park wake cannot be deduplicated by config fingerprints.
It remains pending until a proposal can be constructed without contention. -/
def sendRecoveredProposal (changed recoveryPending : Bool) : Bool := changed || recoveryPending

theorem parked_recovery_not_deduplicated (changed : Bool) :
    sendRecoveredProposal changed true = true := by simp [sendRecoveredProposal]

end RuntimeReconcile.SurfacePublication

namespace RuntimeReconcile.SurfacePublication

/-- Transient plugin publication is a degraded-startup cause even when no
agent is runnable. Permanent invalid surfaces still block all-agent startup. -/
def degradedPublicationFailure (surfaceUnavailable busyCause : Bool) : Bool :=
  surfaceUnavailable && busyCause

theorem all_busy_surfaces_allow_degraded_startup : degradedPublicationFailure true true = true := by decide

theorem permanent_surface_still_blocks : degradedPublicationFailure true false = false := by decide

/-- A non-busy resolve failure must not strand an already-pending recovery
wake after the visibility settle window expires. -/
def retryRecoveryFailure (retryable recoveryPending : Bool) : Bool := retryable || recoveryPending

theorem recovery_wake_survives_resolve_failure (retryable : Bool) :
  retryRecoveryFailure retryable true = true := by simp [retryRecoveryFailure]

end RuntimeReconcile.SurfacePublication
