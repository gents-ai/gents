import Proofs.Runtime.RuntimeReconcile.StartupReadiness
import Proofs.Tools.ToolPolicy.PluginPublication

namespace RuntimeReconcile.SurfacePublication

/-- An exhausted publication wait is not evidence that an agent's desired
configuration is invalid. Refuse that proposal, preserving the active
configuration and its dispatchers; a non-transient failure still demotes. -/
def keepPrevious (busy : Bool) : Bool := busy

theorem busy_never_demotes : keepPrevious true = true := by decide

theorem permanent_failure_does_not_keep_stale_authority :
    keepPrevious false = false := by decide

end RuntimeReconcile.SurfacePublication
