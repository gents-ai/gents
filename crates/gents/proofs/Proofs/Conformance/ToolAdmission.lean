import Proofs.Runtime.RuntimeReconcile.StartupReadiness
import Proofs.Conformance.Family

namespace Conformance.ToolAdmission

open Lean RuntimeReconcile.StartupReadiness

deriving instance ToJson for ToolAdmission, UnavailableReason

structure Input where
  admission : ToolAdmission
  deriving ToJson

structure CascadeOutput where
  runnable : Bool
  reason : Option UnavailableReason
  offeredAsTarget : Bool
  taskActive : Bool
  scheduleActive : Bool
  scheduleUnavailable : Bool
  deriving ToJson

structure StandingOutput where
  runnable : Bool
  reason : Option UnavailableReason
  deriving ToJson

structure LoadingOutput where
  runnable : Bool
  reason : Option UnavailableReason
  diagnosticNamesTarget : Bool
  deriving ToJson

/-- The startup snapshot demotes an agent whose surface fails to build and
cascades the demotion to its dependents. -/
@[conformance_family]
def startupCascade : Family Input CascadeOutput where
  inputs := [⟨.builds⟩, ⟨.failsToBuild⟩]
  model := fun i =>
    let standing := admittedStanding i.admission
    let deps := dependents standing
    { runnable := runnable standing, reason := admissionReason i.admission
      offeredAsTarget := deps.offeredAsTarget, taskActive := deps.taskActive
      scheduleActive := deps.scheduleActive, scheduleUnavailable := deps.scheduleUnavailable }

/-- Document loading resolves agent targets; a target that does not resolve
leaves the agent that names it unavailable. -/
@[conformance_family]
def documentLoading : Family Input LoadingOutput where
  inputs := [⟨.builds⟩, ⟨.unresolvedTarget⟩]
  model := fun i =>
    { runnable := runnable (admittedStanding i.admission), reason := admissionReason i.admission
      diagnosticNamesTarget := diagnosticNamesTarget i.admission }

@[conformance_family]
def watcherWake : Family Input StandingOutput where
  inputs := [⟨.builds⟩, ⟨.failsToBuild⟩]
  model := fun i =>
    { runnable := runnable (afterWake i.admission), reason := admissionReason i.admission }

end Conformance.ToolAdmission
